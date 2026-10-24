//! Real command adapters for live non-Git providers; no shell commands or invented results.
use crate::provider::Provider;
use rustpy_settings::other_vcs::RootSettings;
use std::{
    path::{Path, PathBuf},
    time::Duration,
};
pub mod results;
pub mod runner;
pub mod tfs;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Operation {
    Initialize,
    MapWorkspace,
    Status,
    Commit,
    Log,
    Branches,
    Bookmarks,
    CreateBranch,
    Bookmark,
    Push,
    Pull,
    Incoming,
    Outgoing,
    Merge,
    Graft,
    Rebase,
    Shelve,
    Unshelve,
    QueueInit,
    QueueNew,
    QueuePush,
    QueuePop,
    QueueSeries,
    Annotate,
    Diff,
    Update,
    Checkout,
    Import,
    Revert,
    CopyTag,
    Properties,
    SetProperty,
    Ignore,
    Lock,
    Unlock,
    Cleanup,
    Relocate,
    Changelists,
    AssignChangelist,
    Integrate,
    Login,
    Trust,
    Info,
    Submit,
    Sync,
    Jobs,
    JobDetails,
    SaveJob,
    FixJob,
    UnfixJob,
    CreateChangelist,
    Describe,
    RevisionGraph,
    TimeLapse,
    PrintRevision,
    Add,
    Remove,
    OpenForEdit,
}
impl Operation {
    pub fn available(provider: Provider) -> &'static [Self] {
        use Operation::*;
        match provider {
            Provider::Git => &[],
            Provider::Tfs => &[
                Status,
                Commit,
                Log,
                Update,
                Add,
                Remove,
                OpenForEdit,
                Revert,
                Diff,
                Shelve,
                Unshelve,
                Info,
                Initialize,
                MapWorkspace,
            ],
            Provider::Mercurial => &[
                Status,
                Commit,
                Log,
                Branches,
                CreateBranch,
                Bookmarks,
                Bookmark,
                Push,
                Pull,
                Incoming,
                Outgoing,
                Merge,
                Graft,
                Rebase,
                Shelve,
                Unshelve,
                QueueInit,
                QueueNew,
                QueuePush,
                QueuePop,
                QueueSeries,
                Annotate,
                Diff,
                Update,
                Add,
                Remove,
                Revert,
            ],
            Provider::Subversion => &[
                Status,
                Info,
                Checkout,
                Import,
                Update,
                Commit,
                Revert,
                Log,
                Annotate,
                Diff,
                Branches,
                CopyTag,
                Merge,
                Integrate,
                Properties,
                SetProperty,
                Ignore,
                Lock,
                Unlock,
                Cleanup,
                Relocate,
                Changelists,
                AssignChangelist,
                Add,
                Remove,
            ],
            Provider::Perforce => &[
                Info,
                Login,
                Trust,
                Status,
                Branches,
                Changelists,
                CreateChangelist,
                AssignChangelist,
                Submit,
                Sync,
                Jobs,
                JobDetails,
                SaveJob,
                FixJob,
                UnfixJob,
                Shelve,
                Unshelve,
                Describe,
                Annotate,
                Diff,
                RevisionGraph,
                TimeLapse,
                PrintRevision,
                OpenForEdit,
                Add,
                Remove,
                Revert,
                Lock,
                Unlock,
                Integrate,
            ],
        }
    }
    pub fn label(self) -> &'static str {
        use Operation::*;
        match self {
            Initialize => "Create TFVC workspace",
            MapWorkspace => "Map TFVC workspace",
            Status => "Local Changes",
            Commit => "Commit",
            Log => "History",
            Branches => "Branches",
            Bookmarks => "Bookmarks",
            CreateBranch => "Create branch",
            Bookmark => "Create bookmark",
            Push => "Push",
            Pull => "Pull",
            Incoming => "Incoming",
            Outgoing => "Outgoing",
            Merge => "Merge",
            Graft => "Graft",
            Rebase => "Rebase",
            Shelve => "Shelve",
            Unshelve => "Unshelve",
            QueueInit => "Initialize MQ",
            QueueNew => "New MQ patch",
            QueuePush => "Apply MQ patch",
            QueuePop => "Pop MQ patch",
            QueueSeries => "MQ series",
            Annotate => "Annotate",
            Diff => "Diff",
            Update => "Update to…",
            Checkout => "Checkout",
            Import => "Import",
            Revert => "Revert",
            CopyTag => "Create branch / tag",
            Properties => "Properties",
            SetProperty => "Set property",
            Ignore => "Edit svn:ignore",
            Lock => "Lock",
            Unlock => "Unlock",
            Cleanup => "Cleanup",
            Relocate => "Relocate working copy",
            Changelists => "Changelists",
            AssignChangelist => "Move to changelist",
            Integrate => "Integrate",
            Login => "Login",
            Trust => "Trust server fingerprint",
            Info => "Connection information",
            Submit => "Submit",
            Sync => "Sync",
            Jobs => "Jobs",
            JobDetails => "Job form",
            SaveJob => "Save job",
            FixJob => "Link job to changelist",
            UnfixJob => "Unlink job from changelist",
            CreateChangelist => "New changelist",
            Describe => "Describe changelist",
            RevisionGraph => "Revision graph",
            TimeLapse => "Time-lapse view",
            PrintRevision => "File at revision",
            Add => "Add",
            Remove => "Remove",
            OpenForEdit => "Open for edit",
        }
    }
    pub fn mutates(self) -> bool {
        !matches!(
            self,
            Self::Status
                | Self::Log
                | Self::Branches
                | Self::Bookmarks
                | Self::Incoming
                | Self::Outgoing
                | Self::QueueSeries
                | Self::Annotate
                | Self::Diff
                | Self::Properties
                | Self::Changelists
                | Self::Info
                | Self::Jobs
                | Self::JobDetails
                | Self::Describe
                | Self::RevisionGraph
                | Self::TimeLapse
                | Self::PrintRevision
        )
    }
    pub fn destructive(self) -> bool {
        matches!(
            self,
            Self::Revert | Self::Relocate | Self::Remove | Self::QueuePop
        )
    }
    pub fn needs_target(self) -> bool {
        matches!(
            self,
            Self::Annotate
                | Self::Diff
                | Self::Revert
                | Self::Properties
                | Self::SetProperty
                | Self::Ignore
                | Self::Lock
                | Self::Unlock
                | Self::RevisionGraph
                | Self::TimeLapse
                | Self::PrintRevision
                | Self::Add
                | Self::Remove
                | Self::OpenForEdit
                | Self::AssignChangelist
        )
    }
}
#[derive(Clone, Debug, Default)]
pub struct Arguments {
    pub paths: Vec<PathBuf>,
    pub message: String,
    pub revision: String,
    pub name: String,
    pub source: String,
    pub destination: String,
    pub value: String,
    pub change: String,
    pub force: bool,
    pub dry_run: bool,
}
#[derive(Clone, Default)]
pub struct Credentials {
    pub username: String,
    pub password: String,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    TfsStatus,
    TfsHistory,
    TfsWorkspaces,
    Text,
    HgStatus,
    HgJson,
    SvnStatus,
    SvnLog,
    SvnInfo,
    SvnList,
    SvnProperties,
    P4Tagged,
    Annotation,
    UnifiedDiff,
    RevisionGraph,
    TimeLapse,
}
#[derive(Clone, Debug)]
pub struct CommandPlan {
    pub tfs_authority: Option<tfs::Authority>,
    pub root: PathBuf,
    pub provider: Provider,
    pub operation: Operation,
    pub executable: String,
    pub args: Vec<String>,
    pub stdin: Option<String>,
    pub format: Format,
    pub timeout: Duration,
    pub success_codes: Vec<i32>,
}
impl CommandPlan {
    pub fn display(&self) -> String {
        format!("{} {}", self.executable, self.args.join(" "))
    }
}
fn one_line(value: &str, label: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        !value.contains(['\r', '\n', '\0']),
        "{label} must contain a single line"
    );
    Ok(())
}
fn required(value: &str, label: &str) -> anyhow::Result<String> {
    one_line(value, label)?;
    anyhow::ensure!(!value.trim().is_empty(), "Enter {label}");
    anyhow::ensure!(
        !value.trim_start().starts_with('-'),
        "{label} cannot start with an option prefix"
    );
    Ok(value.trim().to_string())
}
pub fn validate_endpoint(endpoint: &str) -> anyhow::Result<()> {
    one_line(endpoint, "Server")?;
    anyhow::ensure!(
        !endpoint.trim_start().starts_with('-'),
        "A server or remote cannot start with an option prefix"
    );
    if let Ok(url) = url::Url::parse(endpoint) {
        anyhow::ensure!(
            url.password().is_none(),
            "Use the password field rather than embedding a password in a repository URL"
        );
    }
    Ok(())
}
fn change(value: &str) -> anyhow::Result<String> {
    let value = required(value, "a numbered changelist")?;
    anyhow::ensure!(
        value.parse::<u64>().is_ok_and(|v| v > 0),
        "Enter a positive changelist number"
    );
    Ok(value)
}
fn paths(root: &Path, args: &Arguments, required: bool) -> anyhow::Result<Vec<String>> {
    if args.paths.is_empty() {
        anyhow::ensure!(!required, "Select a file or directory");
        return Ok(Vec::new());
    }
    args.paths
        .iter()
        .map(|path| {
            let path = if path.is_absolute() {
                path.clone()
            } else {
                root.join(path)
            };
            anyhow::ensure!(
                path.starts_with(root)
                    && !path
                        .components()
                        .any(|c| matches!(c, std::path::Component::ParentDir)),
                "The file is outside the captured provider root"
            );
            one_line(&path.to_string_lossy(), "File path")?;
            Ok(path.to_string_lossy().into_owned())
        })
        .collect()
}
pub fn plan(
    provider: Provider,
    root: &Path,
    operation: Operation,
    args: &Arguments,
    settings: &RootSettings,
    executable: &str,
) -> anyhow::Result<CommandPlan> {
    if provider == Provider::Tfs {
        return tfs::plan(root, operation, args, settings, executable);
    }
    use Operation::*;
    anyhow::ensure!(
        provider != Provider::Git && Operation::available(provider).contains(&operation),
        "This operation is not supported by the selected provider"
    );
    anyhow::ensure!(root.is_absolute(), "The provider root must be absolute");
    validate_endpoint(&settings.endpoint)?;
    for (value, label) in [
        (&settings.endpoint, "Server"),
        (&settings.username, "User"),
        (&settings.client, "Workspace"),
    ] {
        one_line(value, label)?;
    }
    let targets = if provider == Provider::Perforce
        && args
            .paths
            .iter()
            .any(|path| path.to_string_lossy().starts_with("//"))
    {
        args.paths
            .iter()
            .map(|path| {
                let text = path.to_string_lossy();
                if text.starts_with("//") {
                    one_line(&text, "Depot path")?;
                    Ok(text.into_owned())
                } else {
                    paths(
                        root,
                        &Arguments {
                            paths: vec![path.clone()],
                            ..Default::default()
                        },
                        true,
                    )
                    .map(|mut paths| paths.remove(0))
                }
            })
            .collect::<anyhow::Result<Vec<_>>>()?
    } else {
        paths(root, args, operation.needs_target())?
    };
    if matches!(
        operation,
        Operation::Annotate
            | Operation::TimeLapse
            | Operation::PrintRevision
            | Operation::RevisionGraph
    ) {
        anyhow::ensure!(
            targets.len() == 1,
            "Select a single file for annotation or revision navigation"
        );
    }
    let mut out = CommandPlan {
        tfs_authority: None,
        root: root.to_path_buf(),
        provider,
        operation,
        executable: if executable.trim().is_empty() {
            match provider {
                Provider::Mercurial => "hg",
                Provider::Subversion => "svn",
                Provider::Perforce => "p4",
                Provider::Tfs => unreachable!(),
                Provider::Git => unreachable!(),
            }
            .into()
        } else {
            executable.trim().into()
        },
        args: Vec::new(),
        stdin: None,
        format: Format::Text,
        timeout: Duration::from_secs(settings.timeout_seconds.clamp(10, 3600) as u64),
        success_codes: vec![0],
    };
    let revision = || required(&args.revision, "a revision or range");
    let name = || required(&args.name, "a name");
    let source = || required(&args.source, "a source URL, path or changelist");
    let destination = || required(&args.destination, "a destination URL or path");
    let message = || {
        anyhow::ensure!(
            !args.message.trim().is_empty(),
            "Enter a commit message or description"
        );
        Ok::<String, anyhow::Error>(args.message.clone())
    };
    let limit = settings.history_limit.clamp(1, 10_000).to_string();
    match provider {
        Provider::Mercurial => {
            out.args = vec![
                "--config".into(),
                "ui.interactive=False".into(),
                "--config".into(),
                "progress.assume-tty=True".into(),
            ];
            if matches!(
                operation,
                Rebase
                    | Shelve
                    | Unshelve
                    | QueueInit
                    | QueueNew
                    | QueuePush
                    | QueuePop
                    | QueueSeries
            ) {
                let extension = if operation == Rebase {
                    "rebase"
                } else if matches!(operation, Shelve | Unshelve) {
                    "shelve"
                } else {
                    "mq"
                };
                out.args
                    .extend(["--config".into(), format!("extensions.{extension}=")]);
            }
            let mut command = match operation {
                Status => {
                    out.format = Format::HgStatus;
                    vec!["status".into(), "-0".into()]
                }
                Commit => vec!["commit".into(), "--message".into(), message()?],
                Log | Incoming | Outgoing => {
                    out.format = Format::HgJson;
                    if matches!(operation, Incoming | Outgoing) {
                        out.success_codes.push(1);
                    }
                    let command = match operation {
                        Incoming => "incoming",
                        Outgoing => "outgoing",
                        _ => "log",
                    };
                    let mut result = vec![
                        command.into(),
                        "--template".into(),
                        "json".into(),
                        "--limit".into(),
                        limit.clone(),
                    ];
                    if operation != Log && !settings.endpoint.is_empty() {
                        result.push(settings.endpoint.clone());
                    }
                    result
                }
                Branches | Bookmarks => {
                    out.format = Format::HgJson;
                    vec![
                        if operation == Branches {
                            "branches"
                        } else {
                            "bookmarks"
                        }
                        .into(),
                        "--template".into(),
                        "json".into(),
                    ]
                }
                CreateBranch => vec!["branch".into(), name()?],
                Bookmark => vec!["bookmark".into(), name()?],
                Push | Pull => {
                    let mut result = vec![if operation == Push { "push" } else { "pull" }.into()];
                    if operation == Pull && settings.hg_update_after_pull {
                        result.push("--update".into());
                    }
                    if !settings.endpoint.is_empty() {
                        result.push(settings.endpoint.clone());
                    }
                    result
                }
                Merge => vec!["merge".into(), "--rev".into(), revision()?],
                Graft => vec!["graft".into(), "--rev".into(), revision()?],
                Rebase => vec!["rebase".into(), "--dest".into(), revision()?],
                Shelve => vec!["shelve".into(), "--name".into(), name()?],
                Unshelve => vec!["unshelve".into(), "--name".into(), name()?],
                QueueInit => vec!["qinit".into()],
                QueueNew => vec!["qnew".into(), "--message".into(), message()?, name()?],
                QueuePush => {
                    let mut result = vec!["qpush".into()];
                    if !args.name.is_empty() {
                        result.push(name()?);
                    }
                    result
                }
                QueuePop => vec!["qpop".into()],
                QueueSeries => vec!["qseries".into()],
                Annotate => {
                    out.format = Format::Annotation;
                    vec![
                        "annotate".into(),
                        "--number".into(),
                        "--changeset".into(),
                        "--user".into(),
                        "--date".into(),
                    ]
                }
                Diff => {
                    out.format = Format::UnifiedDiff;
                    let mut result = vec!["diff".into()];
                    if !args.revision.is_empty() {
                        result.extend(["--rev".into(), revision()?]);
                    }
                    result
                }
                Update => {
                    let mut result = vec!["update".into()];
                    if !args.revision.is_empty() {
                        result.extend(["--rev".into(), revision()?]);
                    }
                    if args.force {
                        result.push("--clean".into());
                    }
                    result
                }
                Add => vec!["add".into()],
                Remove => vec!["remove".into(), "--after".into()],
                Revert => vec!["revert".into(), "--no-backup".into()],
                _ => unreachable!(),
            };
            if !targets.is_empty()
                && !matches!(
                    operation,
                    Push | Pull
                        | Incoming
                        | Outgoing
                        | Merge
                        | Graft
                        | Rebase
                        | Update
                        | QueueInit
                        | QueuePush
                        | QueuePop
                        | QueueSeries
                        | CreateBranch
                        | Bookmark
                        | Branches
                        | Bookmarks
                )
            {
                command.push("--".into());
                command.extend(targets);
            }
            // Mercurial's JSON changeset formatter honors --quiet by returning
            // only rev/node, omitting descriptions, authors and branch data.
            // Machine-readable output already suppresses the prose formatter.
            if out.format == Format::HgJson {
                // Incoming/outgoing still emit status prose. Keep that on the
                // separately captured stderr channel, leaving stdout pure JSON.
                out.args
                    .extend(["--config".into(), "ui.message-output=stderr".into()]);
            }
            out.args.extend(command);
        }
        Provider::Subversion => {
            out.args = vec!["--non-interactive".into()];
            if !settings.username.is_empty() {
                out.args
                    .extend(["--username".into(), settings.username.clone()]);
            }
            if !settings.allow_auth_cache {
                out.args.push("--no-auth-cache".into());
            }
            if !settings.svn_ca_file.is_empty() {
                one_line(&settings.svn_ca_file, "CA file")?;
                out.args.extend([
                    "--config-option".into(),
                    format!(
                        "servers:global:ssl-authority-files={}",
                        settings.svn_ca_file
                    ),
                ]);
            }
            let mut command = match operation {
                Status | Changelists => {
                    out.format = Format::SvnStatus;
                    vec!["status".into(), "--xml".into()]
                }
                Info => {
                    out.format = Format::SvnInfo;
                    vec!["info".into(), "--xml".into(), ".".into()]
                }
                Checkout => vec!["checkout".into(), source()?, destination()?],
                Import => vec![
                    "import".into(),
                    if targets.is_empty() {
                        root.to_string_lossy().into_owned()
                    } else {
                        targets[0].clone()
                    },
                    destination()?,
                    "--message".into(),
                    message()?,
                ],
                Update => {
                    let mut result = vec!["update".into()];
                    if !args.revision.is_empty() {
                        result.extend(["--revision".into(), revision()?]);
                    }
                    result
                }
                Commit => vec!["commit".into(), "--message".into(), message()?],
                Revert => vec!["revert".into(), "--recursive".into()],
                Log => {
                    out.format = Format::SvnLog;
                    let mut result = vec![
                        "log".into(),
                        "--xml".into(),
                        "--verbose".into(),
                        "--limit".into(),
                        limit.clone(),
                    ];
                    if !args.source.is_empty() {
                        result.push(source()?);
                    } else if !targets.is_empty() {
                        result.push("--".into());
                        result.extend(targets.iter().cloned());
                    }
                    result
                }
                Annotate => {
                    out.format = Format::Annotation;
                    let mut result = vec!["annotate".into(), "--verbose".into()];
                    if !args.revision.is_empty() {
                        result.extend(["--revision".into(), revision()?]);
                    }
                    result
                }
                Diff => {
                    out.format = Format::UnifiedDiff;
                    let mut result = vec!["diff".into()];
                    if !args.revision.is_empty() {
                        result.extend(["--revision".into(), revision()?]);
                    }
                    result
                }
                Branches => {
                    out.format = Format::SvnList;
                    vec!["list".into(), "--xml".into(), source()?]
                }
                CopyTag => vec![
                    "copy".into(),
                    source()?,
                    destination()?,
                    "--message".into(),
                    message()?,
                ],
                Merge | Integrate => {
                    let mut result = vec!["merge".into()];
                    if !args.revision.is_empty() {
                        result.extend(["--revision".into(), revision()?]);
                    }
                    if args.dry_run {
                        result.push("--dry-run".into());
                    }
                    result.push(source()?);
                    result
                }
                Properties => {
                    out.format = Format::SvnProperties;
                    vec!["proplist".into(), "--xml".into(), "--verbose".into()]
                }
                SetProperty | Ignore => vec![
                    "propset".into(),
                    "--".into(),
                    if operation == Ignore {
                        "svn:ignore".into()
                    } else {
                        name()?
                    },
                    args.value.clone(),
                ],
                Lock => vec!["lock".into(), "--message".into(), args.message.clone()],
                Unlock => {
                    let mut result = vec!["unlock".into()];
                    if args.force {
                        result.push("--force".into());
                    }
                    result
                }
                Cleanup => vec!["cleanup".into()],
                Relocate => vec!["relocate".into(), source()?, destination()?],
                AssignChangelist => {
                    if args.name.trim().is_empty() {
                        vec!["changelist".into(), "--remove".into()]
                    } else {
                        vec!["changelist".into(), name()?]
                    }
                }
                Add => vec!["add".into(), "--parents".into()],
                Remove => vec!["delete".into(), "--force".into()],
                _ => unreachable!(),
            };
            if !targets.is_empty()
                && !matches!(
                    operation,
                    Checkout | Import | Info | Branches | CopyTag | Relocate | Log
                )
            {
                if !command.iter().any(|part| part == "--") {
                    command.push("--".into());
                }
                command.extend(targets);
            }
            out.args.extend(command);
        }
        Provider::Perforce => {
            out.args = vec!["-d".into(), root.to_string_lossy().into_owned()];
            for (flag, value) in [
                ("-p", &settings.endpoint),
                ("-u", &settings.username),
                ("-c", &settings.client),
            ] {
                if !value.is_empty() {
                    out.args.extend([flag.into(), value.clone()]);
                }
            }
            let tagged = matches!(
                operation,
                Info | Status | Branches | Changelists | Jobs | Describe | RevisionGraph
            );
            if tagged {
                out.args.push("-ztag".into());
                out.format = if operation == RevisionGraph {
                    Format::RevisionGraph
                } else {
                    Format::P4Tagged
                };
            }
            let mut command = match operation {
                Info => vec!["info".into()],
                Login => vec!["login".into()],
                Trust => {
                    let mut result = vec![
                        "trust".into(),
                        "-i".into(),
                        required(&args.value, "the verified server fingerprint")?,
                    ];
                    if args.force {
                        result.push("-f".into());
                    }
                    result
                }
                Branches => vec!["branches".into(), "-m".into(), limit.clone()],
                Status => vec!["opened".into()],
                Changelists => {
                    let mut result = vec![
                        "changes".into(),
                        "-s".into(),
                        "pending".into(),
                        "-m".into(),
                        limit.clone(),
                    ];
                    if !settings.client.is_empty() {
                        result.extend(["-c".into(), settings.client.clone()]);
                    }
                    result
                }
                CreateChangelist => {
                    let description = message()?
                        .lines()
                        .map(|line| format!("\t{line}\n"))
                        .collect::<String>();
                    out.stdin = Some(format!(
                        "Change: new\nClient: {}\nUser: {}\nDescription:\n{}",
                        required(&settings.client, "a Perforce workspace")?,
                        required(&settings.username, "a Perforce user")?,
                        description
                    ));
                    vec!["change".into(), "-i".into()]
                }
                FixJob | UnfixJob => {
                    let mut result = vec!["fix".into(), "-c".into(), change(&args.change)?];
                    if operation == UnfixJob {
                        result.push("-d".into());
                    }
                    result.push(name()?);
                    result
                }
                AssignChangelist => vec!["reopen".into(), "-c".into(), change(&args.change)?],
                Submit => {
                    if !args.change.is_empty() {
                        vec!["submit".into(), "-c".into(), change(&args.change)?]
                    } else {
                        anyhow::ensure!(targets.len()<=1,"Move selected files into a numbered changelist before submitting multiple files");
                        vec!["submit".into(), "-d".into(), message()?]
                    }
                }
                Sync => {
                    let mut result = vec!["sync".into()];
                    if args.force {
                        result.push("-f".into());
                    }
                    if !args.revision.is_empty() {
                        let target = targets.first().cloned().unwrap_or_else(|| "...".into());
                        let revision = revision()?;
                        result.push(format!(
                            "{target}{}",
                            if revision.starts_with(['#', '@']) {
                                revision
                            } else {
                                format!("@{revision}")
                            }
                        ));
                    }
                    result
                }
                Jobs => {
                    let mut result = vec!["jobs".into(), "-m".into(), limit.clone()];
                    if !args.name.is_empty() {
                        result.extend(["-e".into(), name()?]);
                    }
                    result
                }
                JobDetails => vec!["job".into(), "-o".into(), name()?],
                SaveJob => {
                    anyhow::ensure!(
                        !args.value.trim().is_empty(),
                        "Load or enter a Perforce job form"
                    );
                    out.stdin = Some(args.value.clone());
                    vec!["job".into(), "-i".into()]
                }
                Shelve => vec!["shelve".into(), "-c".into(), change(&args.change)?],
                Unshelve => {
                    let mut result = vec!["unshelve".into(), "-s".into(), change(&args.source)?];
                    if !args.change.is_empty() {
                        result.extend(["-c".into(), change(&args.change)?]);
                    }
                    result
                }
                Describe => vec!["describe".into(), "-s".into(), change(&args.change)?],
                Annotate | TimeLapse => {
                    out.format = if operation == TimeLapse {
                        Format::TimeLapse
                    } else {
                        Format::Annotation
                    };
                    vec!["annotate".into(), "-u".into(), "-c".into()]
                }
                PrintRevision => {
                    out.format = Format::Text;
                    vec!["print".into(), "-q".into()]
                }
                Diff => {
                    out.format = Format::UnifiedDiff;
                    vec!["diff".into(), "-du".into()]
                }
                RevisionGraph => vec![
                    "filelog".into(),
                    "-i".into(),
                    "-l".into(),
                    "-m".into(),
                    limit.clone(),
                ],
                OpenForEdit => vec!["edit".into()],
                Add => vec!["add".into()],
                Remove => vec!["delete".into(), "-k".into()],
                Revert => vec!["revert".into()],
                Lock => vec!["lock".into()],
                Unlock => vec!["unlock".into()],
                Integrate => vec!["integrate".into(), source()?, destination()?],
                _ => unreachable!(),
            };
            if !targets.is_empty()
                && !matches!(
                    operation,
                    Login
                        | Trust
                        | Branches
                        | FixJob
                        | UnfixJob
                        | Info
                        | Changelists
                        | Jobs
                        | JobDetails
                        | SaveJob
                        | CreateChangelist
                        | Describe
                        | Integrate
                )
                && !(operation == Sync && !args.revision.is_empty())
                && !(operation == Submit && !args.change.is_empty())
            {
                for target in targets {
                    command.push(
                        if matches!(operation, Annotate | TimeLapse | PrintRevision)
                            && !args.revision.is_empty()
                        {
                            format!("{target}#{}", revision()?.trim_start_matches('#'))
                        } else {
                            target
                        },
                    );
                }
            }
            out.args.extend(command);
        }
        Provider::Git => unreachable!(),
        Provider::Tfs => unreachable!(),
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn command(provider: Provider, operation: Operation, args: Arguments) -> CommandPlan {
        plan(
            provider,
            Path::new("/workspace"),
            operation,
            &args,
            &RootSettings {
                client: "client".into(),
                username: "developer".into(),
                ..Default::default()
            },
            "provider",
        )
        .unwrap()
    }
    #[test]
    #[cfg(unix)] // Unix fixture paths, tools or process semantics
    fn mercurial_history_requests_complete_json_changeset_metadata() {
        for operation in [Operation::Log, Operation::Incoming, Operation::Outgoing] {
            let plan = command(Provider::Mercurial, operation, Arguments::default());
            assert_eq!(plan.format, Format::HgJson);
            assert!(!plan.args.iter().any(|arg| arg == "--quiet" || arg == "-q"));
            assert!(plan
                .args
                .windows(2)
                .any(|pair| { pair[0] == "--config" && pair[1] == "ui.message-output=stderr" }));
            let history = results::decode(
                Provider::Mercurial,
                Path::new("/workspace"),
                plan.format,
                operation,
                r#"[{"rev":0,"node":"0123456789abcdef","branch":"default","user":"Owned Author","desc":"owned first42\nsecond line"}]"#,
            )
            .unwrap();
            assert_eq!(history.rows[0].label, "owned first42");
            assert_eq!(history.rows[0].fields["desc"], "owned first42\nsecond line");
            assert!(history.rows[0].detail.contains("Owned Author"));
            assert_eq!(
                history.rows[0].revision.as_deref(),
                Some("0123456789abcdef")
            );
        }
    }
    #[test]
    #[cfg(unix)] // Unix fixture paths, tools or process semantics
    fn mercurial_incoming_preserves_no_changes_success_and_mq_is_explicit() {
        let incoming = command(
            Provider::Mercurial,
            Operation::Incoming,
            Arguments::default(),
        );
        assert_eq!(incoming.success_codes, vec![0, 1]);
        let mq = command(
            Provider::Mercurial,
            Operation::QueueNew,
            Arguments {
                name: "fix.patch".into(),
                message: "Fix selected files".into(),
                paths: vec![PathBuf::from("/workspace/file with spaces.py")],
                ..Default::default()
            },
        );
        assert!(mq.args.iter().any(|arg| arg.contains("extensions.mq")));
        assert!(mq.args.iter().any(|arg| arg == "qnew"));
        assert!(mq
            .args
            .iter()
            .any(|arg| arg == "/workspace/file with spaces.py"));
    }
    #[test]
    #[cfg(unix)] // Unix fixture paths, tools or process semantics
    fn svn_history_uses_selected_file_and_ignore_preserves_multiline_value() {
        let history = command(
            Provider::Subversion,
            Operation::Log,
            Arguments {
                paths: vec![PathBuf::from("/workspace/example.py")],
                ..Default::default()
            },
        );
        assert!(history
            .args
            .ends_with(&["--".into(), "/workspace/example.py".into()]));
        let ignore = command(
            Provider::Subversion,
            Operation::Ignore,
            Arguments {
                paths: vec![PathBuf::from("/workspace")],
                value: "*.pyc\n__pycache__".into(),
                ..Default::default()
            },
        );
        assert!(ignore.args.iter().any(|arg| arg == "*.pyc\n__pycache__"));
        assert_eq!(
            ignore
                .args
                .iter()
                .filter(|arg| arg.as_str() == "--")
                .count(),
            1
        );
    }
    #[test]
    #[cfg(unix)] // Unix fixture paths, tools or process semantics
    fn perforce_forms_use_stdin_and_job_links_do_not_append_files() {
        let form = command(
            Provider::Perforce,
            Operation::CreateChangelist,
            Arguments {
                message: "First line\nSecond line".into(),
                ..Default::default()
            },
        );
        assert!(form.stdin.as_deref().unwrap().contains("\tSecond line\n"));
        assert!(!form.args.iter().any(|arg| arg.contains("First line")));
        let fix = command(
            Provider::Perforce,
            Operation::FixJob,
            Arguments {
                name: "job00001".into(),
                change: "42".into(),
                paths: vec!["/workspace/a.py".into()],
                ..Default::default()
            },
        );
        assert!(fix
            .args
            .ends_with(&["fix".into(), "-c".into(), "42".into(), "job00001".into()]));
    }
    #[test]
    fn captured_roots_reject_other_repository_files() {
        assert!(plan(
            Provider::Mercurial,
            Path::new("/workspace"),
            Operation::Add,
            &Arguments {
                paths: vec!["/different/file.py".into()],
                ..Default::default()
            },
            &RootSettings::default(),
            "hg"
        )
        .is_err());
    }
}

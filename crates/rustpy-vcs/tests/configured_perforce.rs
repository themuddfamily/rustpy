//! ROOT-admitted genuine disposable Perforce server; never starts/downloads tools.
//! Descriptor (private JSON): source, token, endpoint=tcp:127.0.0.1:PORT,
//! user, server_root, server_pid, server_start, password_file, p4/p4d{path,sha256}.
//! RUSTPY_P4_ROOT is its existing0700 owned case; owner-token contains token.
//! ROOT owns a fresh server/security=3/user/password and continuous PID/start
//! descendant retirement. This test owns only its two workspace directories and
//! retained public ProviderJobs. No shared/global server/account/config changes.
//! Official command contracts: help.perforce.com/.../CmdRef/p4_{login,change,
//! shelve,unshelve,job,fix,annotate,filelog,print}.html. Local Linux acceptance
//! only; no TLS, external accounts, platform or whole-roadmap conclusion.
#![cfg(target_os = "linux")]
use rustpy_settings::other_vcs::RootSettings;
use rustpy_vcs::{
    other_vcs::{
        self,
        results::Data,
        runner::{Completed, Event, ProviderJob},
        Arguments, CommandPlan, Credentials, Format, Operation,
    },
    provider::{Integration, Provider},
    GitEnv,
};
use serde::Deserialize;
use std::{
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
#[derive(Deserialize)]
struct Tool {
    path: PathBuf,
    sha256: String,
}
#[derive(Deserialize)]
struct Descriptor {
    unicode: bool,
    source: String,
    token: String,
    endpoint: String,
    user: String,
    server_root: PathBuf,
    server_pid: u32,
    server_start: String,
    password_file: PathBuf,
    p4: Tool,
    p4d: Tool,
}
fn required(name: &str) -> String {
    let value = std::env::var(name).unwrap_or_else(|_| panic!("explicit {name} required"));
    assert!(!value.is_empty() && !value.contains(['\r', '\n', '\0']));
    value
}
fn private(path: &Path, root: &Path, maximum: u64) -> PathBuf {
    assert!(path.is_absolute() && !path.is_symlink());
    let path = path.canonicalize().unwrap();
    assert!(path.starts_with(root));
    let info = std::fs::metadata(&path).unwrap();
    assert_eq!(info.uid(), 0);
    assert_eq!(info.mode() & 0o077, 0, "private ROOT authority required");
    assert!(info.is_file() && info.len() <= maximum);
    path
}
fn hash(path: &Path) -> String {
    rustpy_net::x509::sha256(&std::fs::read(path).unwrap())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
struct Job(ProviderJob);
impl Drop for Job {
    fn drop(&mut self) {
        self.0.stop();
        while !self.0.finished() {
            self.0.poll();
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}
struct Case {
    // All jobs are scoped within execute() and physically joined before this
    // TempDir can remove workspace files, on normal return and assertion panic.
    directory: tempfile::TempDir,
    admitted: Descriptor,
    environment: GitEnv,
    password: String,
}
impl Case {
    fn admitted() -> Self {
        assert_eq!(required("RUSTPY_P4_ADMITTED"), "yes");
        let root = PathBuf::from(required("RUSTPY_P4_ROOT"));
        assert!(root.is_absolute() && root.is_dir() && !root.is_symlink());
        let root = root.canonicalize().unwrap();
        let info = std::fs::metadata(&root).unwrap();
        assert_eq!((info.uid(), info.mode() & 0o077), (0, 0));
        let descriptor = private(
            &PathBuf::from(required("RUSTPY_P4_DESCRIPTOR")),
            &root,
            16384,
        );
        let admitted: Descriptor =
            serde_json::from_slice(&std::fs::read(descriptor).unwrap()).unwrap();
        assert!(admitted.unicode, "explicit UTF-8 server authority required");
        assert_eq!(admitted.source, required("RUSTPY_P4_SOURCE"));
        assert!(
            admitted.source.len() == 40 && admitted.source.bytes().all(|b| b.is_ascii_hexdigit())
        );
        assert!(
            admitted.token.len() >= 16 && admitted.token.bytes().all(|b| b.is_ascii_alphanumeric())
        );
        assert_eq!(admitted.token, required("RUSTPY_P4_TOKEN"));
        assert_eq!(
            std::fs::read_to_string(private(&root.join("owner-token"), &root, 256)).unwrap(),
            admitted.token
        );
        let port = admitted
            .endpoint
            .strip_prefix("tcp:127.0.0.1:")
            .unwrap()
            .parse::<u16>()
            .unwrap();
        assert!(port > 1024);
        assert!(
            !admitted.user.is_empty()
                && admitted
                    .user
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_')
        );
        for (tool, expected) in [
            (
                &admitted.p4,
                "8f3c2688c537f614fd5ae94a1edb0110c35a08bca4bc856c4cdc5e29b68e457e",
            ),
            (
                &admitted.p4d,
                "b18232bd835f0232b967e6bd3459c15da8ec69ca071d3efdf98327eca4315440",
            ),
        ] {
            let path = private(&tool.path, &root, 64 * 1024 * 1024);
            assert_eq!(tool.sha256, expected);
            assert_eq!(hash(&path), expected);
        }
        assert!(
            admitted.server_root.is_absolute()
                && admitted
                    .server_root
                    .canonicalize()
                    .unwrap()
                    .starts_with(&root)
        );
        assert_eq!(
            std::fs::read_link(format!("/proc/{}/exe", admitted.server_pid)).unwrap(),
            admitted.p4d.path.canonicalize().unwrap()
        );
        let stat = std::fs::read_to_string(format!("/proc/{}/stat", admitted.server_pid)).unwrap();
        assert_eq!(
            stat.rsplit_once(") ")
                .unwrap()
                .1
                .split_whitespace()
                .nth(19)
                .unwrap(),
            admitted.server_start
        );
        let password =
            std::fs::read_to_string(private(&admitted.password_file, &root, 1024)).unwrap();
        assert!(!password.is_empty() && !password.contains(['\r', '\n', '\0']));
        let directory = tempfile::Builder::new()
            .prefix("owned-p4-")
            .tempdir_in(&root)
            .unwrap();
        let config = directory.path().join("p4config");
        let environ = directory.path().join("p4enviro");
        std::fs::write(&config, "").unwrap();
        std::fs::write(&environ, "").unwrap();
        let environment = GitEnv {
            vars: vec![
                ("P4CONFIG".into(), config.to_str().unwrap().into()),
                ("P4ENVIRO".into(), environ.to_str().unwrap().into()),
                ("P4ALIASES".into(), config.to_str().unwrap().into()),
                ("P4IGNORE".into(), config.to_str().unwrap().into()),
                (
                    "P4TICKETS".into(),
                    directory.path().join("tickets").to_str().unwrap().into(),
                ),
                (
                    "P4TRUST".into(),
                    directory.path().join("trust").to_str().unwrap().into(),
                ),
                ("P4CHARSET".into(), "utf8".into()),
                ("P4PASSWD".into(), "".into()),
                ("P4LOGINSSO".into(), "".into()),
                ("P4COMMANDCHARSET".into(), "".into()),
                ("P4HOST".into(), "".into()),
            ],
        };
        Self {
            directory,
            admitted,
            environment,
            password,
        }
    }
    fn execute(&self, plan: CommandPlan, login: bool) -> (Completed, Data) {
        let format = plan.format;
        let operation = plan.operation;
        let root = plan.root.clone();
        let credentials = if login {
            Credentials {
                password: self.password.clone(),
                ..Default::default()
            }
        } else {
            Credentials::default()
        };
        let mut owner = Job(ProviderJob::start(
            plan,
            credentials,
            self.environment.clone(),
            &self.admitted.endpoint,
        )
        .unwrap());
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            for event in owner.0.poll() {
                if let Event::Done(done) = event {
                    assert!(owner.0.finished() && rustpy_vcs::running_operation(&root).is_none());
                    assert_eq!(done.root, root);
                    assert_eq!(done.provider, Provider::Perforce);
                    assert_eq!(done.operation, operation);
                    assert!(!done.cancelled && !done.truncated);
                    // Never print a genuine password/ticket/token in failure diagnostics.
                    assert!(done.success, "genuine Perforce operation {operation:?} failed (private ROOT logs retained)");
                    let data = other_vcs::results::decode(
                        Provider::Perforce,
                        &root,
                        format,
                        operation,
                        &done.stdout,
                    )
                    .unwrap();
                    return (done, data);
                }
            }
            assert!(Instant::now() < deadline, "owned Perforce request deadline");
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    fn operation(
        &self,
        settings: &RootSettings,
        operation: Operation,
        args: Arguments,
    ) -> (Completed, Data) {
        let plan = other_vcs::plan(
            Provider::Perforce,
            &settings.root,
            operation,
            &args,
            settings,
            self.admitted.p4.path.to_str().unwrap(),
        )
        .unwrap();
        self.execute(plan, operation == Operation::Login)
    }
    fn workspace(&self, suffix: &str) -> RootSettings {
        let root = self.directory.path().join(suffix);
        std::fs::create_dir(&root).unwrap();
        let client = format!("rustpy_{}_{}", self.admitted.token, suffix);
        let settings = RootSettings {
            root: root.clone(),
            endpoint: self.admitted.endpoint.clone(),
            username: self.admitted.user.clone(),
            client: client.clone(),
            timeout_seconds: 20,
            ..Default::default()
        };
        // Login does not depend on a pre-existing client, and uses public production credentials/stdin.
        self.operation(&settings, Operation::Login, Arguments::default());
        let integration = Integration {
            provider: Provider::Perforce,
            root: root.clone(),
            executable: self.admitted.p4.path.to_str().unwrap().into(),
            endpoint: self.admitted.endpoint.clone(),
            workspace: client,
            user: self.admitted.user.clone(),
            depot: "//depot/...".into(),
            ..Default::default()
        };
        for setup in integration.commands().unwrap() {
            assert_eq!(Path::new(&setup.executable), self.admitted.p4.path);
            self.execute(
                CommandPlan {
                    tfs_authority: None,
                    root: setup.cwd,
                    provider: Provider::Perforce,
                    operation: Operation::Info,
                    executable: setup.executable,
                    args: setup.args,
                    stdin: setup.stdin,
                    format: Format::Text,
                    timeout: Duration::from_secs(20),
                    success_codes: vec![0],
                },
                false,
            );
        }
        settings
    }
    fn change(&self, settings: &RootSettings, description: &str) -> String {
        let (done, _) = self.operation(
            settings,
            Operation::CreateChangelist,
            Arguments {
                message: description.into(),
                ..Default::default()
            },
        );
        let change = done
            .stdout
            .lines()
            .find_map(|line| {
                line.strip_prefix("Change ")
                    .and_then(|rest| rest.split_once(' '))
                    .filter(|(_, tail)| *tail == "created.")
                    .map(|(id, _)| id.to_string())
            })
            .expect("actual numbered changelist creation");
        assert!(change.bytes().all(|b| b.is_ascii_digit()));
        let (_, pending) = self.operation(settings, Operation::Changelists, Arguments::default());
        assert!(pending
            .rows
            .iter()
            .any(|row| row.fields.get("change") == Some(&change)
                && row
                    .fields
                    .get("desc")
                    .is_some_and(|text| text.contains(description))));
        change
    }
}
fn paths(path: &Path) -> Arguments {
    Arguments {
        paths: vec![path.into()],
        ..Default::default()
    }
}

#[test]
#[ignore = "ROOT-admitted genuine private p4/p4d with security=3; no provision/startup in fixture"]
fn configured_perforce_public_workflow_preserves_jobs_shelves_and_file_revisions() {
    let case = Case::admitted();
    let a = case.workspace("a");
    let b = case.workspace("b");
    let (_, info) = case.operation(&a, Operation::Info, Arguments::default());
    assert!(info.rows.iter().any(
        |row| row.fields.get("serverRoot").map(PathBuf::from).as_deref()
            == Some(case.admitted.server_root.as_path())
    ));
    assert!(info.rows.iter().any(
        |row| row.fields.get("userName") == Some(&case.admitted.user)
            && row.fields.get("clientName") == Some(&a.client)
    ));
    let tickets = case.directory.path().join("tickets");
    assert!(
        std::fs::metadata(tickets).unwrap().len() > 0,
        "genuine login must persist its private ticket"
    );
    let file = a.root.join("value.txt");
    let copy = b.root.join("value.txt");
    std::fs::write(&file, "42\n").unwrap();
    case.operation(&a, Operation::Add, paths(&file));
    let first = case.change(&a, "owned first42");
    case.operation(
        &a,
        Operation::AssignChangelist,
        Arguments {
            change: first.clone(),
            ..paths(&file)
        },
    );
    let (_, opened) = case.operation(&a, Operation::Status, Arguments::default());
    assert!(opened
        .rows
        .iter()
        .any(|row| row.fields.get("change") == Some(&first)
            && row
                .fields
                .get("depotFile")
                .is_some_and(|path| path == "//depot/value.txt")));
    case.operation(
        &a,
        Operation::Submit,
        Arguments {
            change: first.clone(),
            ..Default::default()
        },
    );
    case.operation(&b, Operation::Sync, Arguments::default());
    assert_eq!(std::fs::read_to_string(&copy).unwrap(), "42\n");
    case.operation(&a, Operation::OpenForEdit, paths(&file));
    std::fs::write(&file, "43\n").unwrap();
    let shelf = case.change(&a, "owned shelf43");
    case.operation(
        &a,
        Operation::AssignChangelist,
        Arguments {
            change: shelf.clone(),
            ..paths(&file)
        },
    );
    case.operation(
        &a,
        Operation::Shelve,
        Arguments {
            change: shelf.clone(),
            ..Default::default()
        },
    );
    case.operation(&a, Operation::Revert, paths(&file));
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "42\n");
    let second = case.change(&b, "owned second43");
    case.operation(
        &b,
        Operation::Unshelve,
        Arguments {
            source: shelf,
            change: second.clone(),
            ..Default::default()
        },
    );
    assert_eq!(std::fs::read_to_string(&copy).unwrap(), "43\n");
    case.operation(
        &b,
        Operation::Submit,
        Arguments {
            change: second.clone(),
            ..Default::default()
        },
    );
    case.operation(&a, Operation::Sync, Arguments::default());
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "43\n");
    let job = format!("rustpy_{}_job", case.admitted.token);
    let form = format!(
        "Job: {job}\nStatus: open\nUser: {}\nDescription:\n\towned job42\n",
        case.admitted.user
    );
    case.operation(
        &a,
        Operation::SaveJob,
        Arguments {
            value: form,
            ..Default::default()
        },
    );
    let (_, jobs) = case.operation(
        &a,
        Operation::Jobs,
        Arguments {
            name: format!("Job={job}"),
            ..Default::default()
        },
    );
    assert!(jobs
        .rows
        .iter()
        .any(|row| row.fields.get("Job") == Some(&job)
            && row
                .fields
                .get("Description")
                .is_some_and(|text| text.contains("owned job42"))));
    let (_, details) = case.operation(
        &a,
        Operation::JobDetails,
        Arguments {
            name: job.clone(),
            ..Default::default()
        },
    );
    assert!(details.raw.contains(&job) && details.raw.contains("owned job42"));
    case.operation(
        &a,
        Operation::FixJob,
        Arguments {
            name: job.clone(),
            change: second.clone(),
            ..Default::default()
        },
    );
    let (_, fixed) = case.operation(
        &a,
        Operation::Describe,
        Arguments {
            change: second.clone(),
            ..Default::default()
        },
    );
    assert!(
        fixed.raw.contains(&job),
        "actual submitted changelist must retain linked job"
    );
    case.operation(
        &a,
        Operation::UnfixJob,
        Arguments {
            name: job.clone(),
            change: second.clone(),
            ..Default::default()
        },
    );
    let (_, unfixed) = case.operation(
        &a,
        Operation::Describe,
        Arguments {
            change: second.clone(),
            ..Default::default()
        },
    );
    assert!(
        !unfixed.raw.contains(&job),
        "actual job link removal must reach server"
    );
    let (_, graph) = case.operation(&a, Operation::RevisionGraph, paths(&file));
    assert!(
        graph
            .graph
            .iter()
            .any(|node| node.file == "//depot/value.txt"
                && node.revision == 1
                && node.change == first),
        "first depot revision missing; raw={:?}, decoded={:?}",
        graph.raw,
        graph.graph
    );
    assert!(
        graph
            .graph
            .iter()
            .any(|node| node.file == "//depot/value.txt"
                && node.revision == 2
                && node.change == second),
        "second depot revision missing; raw={:?}, decoded={:?}",
        graph.raw,
        graph.graph
    );
    assert!(
        graph
            .edges
            .iter()
            .any(|edge| edge.from == "//depot/value.txt#2" && edge.to == "//depot/value.txt#1"),
        "revision edge missing; raw={:?}, decoded={:?}",
        graph.raw,
        graph.edges
    );
    for (revision, content, change) in [("1", "42\n", &first), ("2", "43\n", &second)] {
        let args = Arguments {
            revision: revision.into(),
            ..paths(&file)
        };
        let (_, printed) = case.operation(&a, Operation::PrintRevision, args.clone());
        assert_eq!(printed.raw, content, "actual depot revision bytes");
        for operation in [Operation::Annotate, Operation::TimeLapse] {
            let (_, annotated) = case.operation(&a, operation, args.clone());
            assert!(
                annotated
                    .rows
                    .iter()
                    .any(|row| row.revision.as_ref() == Some(change)
                        && row.label.ends_with(content.trim_end_matches('\n'))),
                "actual changelist annotation for selected file revision"
            );
        }
    }
}

//! Ignored real Hg/SVN acceptance. ROOT admits already installed absolute tools
//! and an existing private parent; no downloads, daemon, mock CLI or protocol.
//! These cases cover local repository workflows, not remote authentication,
//! every optional extension, Perforce, or complete roadmap362 acceptance.
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
use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

fn required(name: &str) -> String {
    let value = std::env::var(name).unwrap_or_else(|_| panic!("explicit {name} required"));
    assert!(!value.is_empty() && !value.contains(['\r', '\n', '\0']));
    value
}
fn executable(name: &str) -> PathBuf {
    let path = PathBuf::from(required(name));
    assert!(
        path.is_absolute() && path.is_file(),
        "real absolute executable required"
    );
    path.canonicalize().unwrap()
}
struct Case {
    directory: tempfile::TempDir,
    provider: Provider,
    executable: PathBuf,
    environment: GitEnv,
}
struct OwnedJob(ProviderJob);
impl Drop for OwnedJob {
    fn drop(&mut self) {
        self.0.stop();
        // A panic cannot tear down provider files under a still-running job.
        // finished() now requires its physical worker/reader joins.
        while !self.0.finished() {
            self.0.poll();
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}
impl Case {
    fn configured(provider: Provider, variable: &str) -> Self {
        assert_eq!(required("RUSTPY_OTHER_VCS_ADMITTED"), "yes");
        let parent = PathBuf::from(required("RUSTPY_OTHER_VCS_ROOT"));
        assert!(parent.is_absolute() && parent.is_dir());
        let executable = executable(variable);
        let directory = tempfile::Builder::new()
            .prefix("owned-hg-svn-")
            .tempdir_in(parent)
            .unwrap();
        let hgrc = directory.path().join("private-hgrc");
        std::fs::create_dir(directory.path().join("svn-config")).unwrap();
        std::fs::write(
            &hgrc,
            "[ui]\nusername = Owned VCS Fixture <owned@example.test>\ninteractive = False\n",
        )
        .unwrap();
        let environment = GitEnv {
            vars: vec![
                ("HGPLAIN".into(), "1".into()),
                ("HGRCPATH".into(), hgrc.to_str().unwrap().into()),
                (
                    "HGUSER".into(),
                    "Owned VCS Fixture <owned@example.test>".into(),
                ),
            ],
        };
        Self {
            directory,
            provider,
            executable,
            environment,
        }
    }
    fn root(&self) -> &Path {
        self.directory.path()
    }
    fn execute(&self, mut plan: CommandPlan, endpoint: &str) -> (Completed, Data) {
        if self.provider == Provider::Subversion
            && Path::new(&plan.executable).canonicalize().unwrap() == self.executable
        {
            // SVN otherwise creates/reads ~/.subversion even for file://.
            // Apply its real global option to setup checkout and planned jobs;
            // svnadmin is a different admitted executable and has no such flag.
            plan.args.splice(
                0..0,
                [
                    "--config-dir".into(),
                    self.root().join("svn-config").to_str().unwrap().into(),
                ],
            );
        }
        let format = plan.format;
        let operation = plan.operation;
        let root = plan.root.clone();
        let mut owner = OwnedJob(
            ProviderJob::start(
                plan,
                Credentials::default(),
                self.environment.clone(),
                endpoint,
            )
            .unwrap(),
        );
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            for event in owner.0.poll() {
                if let Event::Done(done) = event {
                    assert_eq!(done.root, root);
                    assert_eq!(done.provider, self.provider);
                    assert_eq!(done.operation, operation);
                    assert!(owner.0.finished());
                    assert!(rustpy_vcs::running_operation(&root).is_none());
                    assert!(!done.cancelled && !done.truncated);
                    assert!(
                        done.success,
                        "real provider operation failed: {}",
                        done.stderr
                    );
                    let decoded = other_vcs::results::decode(
                        self.provider,
                        &root,
                        format,
                        operation,
                        &done.stdout,
                    )
                    .unwrap();
                    return (done, decoded);
                }
            }
            assert!(
                Instant::now() < deadline,
                "actual provider operation deadline"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    fn operation(
        &self,
        root: &Path,
        settings: &RootSettings,
        operation: Operation,
        arguments: Arguments,
    ) -> (Completed, Data) {
        let plan = other_vcs::plan(
            self.provider,
            root,
            operation,
            &arguments,
            settings,
            self.executable.to_str().unwrap(),
        )
        .unwrap();
        self.execute(plan, &settings.endpoint)
    }
    fn initialize(&self, integration: Integration, svnadmin: Option<&Path>) {
        // Run genuine public integration setup commands through the SAME owned
        // process runner. Never call its older synchronous setup executor.
        for command in integration.commands().unwrap() {
            let program = Path::new(&command.executable).canonicalize().unwrap();
            assert!(program == self.executable || svnadmin == Some(program.as_path()));
            let plan = CommandPlan {
                tfs_authority: None,
                root: command.cwd,
                provider: self.provider,
                operation: Operation::Status,
                executable: command.executable,
                args: command.args,
                stdin: command.stdin,
                format: Format::Text,
                timeout: Duration::from_secs(20),
                success_codes: vec![0],
            };
            self.execute(plan, "");
        }
    }
}
fn paths(path: &Path) -> Arguments {
    Arguments {
        paths: vec![path.to_path_buf()],
        ..Default::default()
    }
}
fn message(value: &str) -> Arguments {
    Arguments {
        message: value.into(),
        ..Default::default()
    }
}
fn settings(root: &Path) -> RootSettings {
    RootSettings {
        root: root.to_path_buf(),
        username: "owned-vcs-fixture".into(),
        timeout_seconds: 20,
        ..Default::default()
    }
}

#[test]
#[ignore = "ROOT-admitted genuine Mercurial executable; two fresh owned local repositories"]
fn configured_mercurial_status_commit_bookmark_pull_push_and_update_use_real_provider() {
    let case = Case::configured(Provider::Mercurial, "RUSTPY_TEST_HG");
    let a = case.root().join("repository-a");
    let b = case.root().join("repository-b");
    for root in [&a, &b] {
        std::fs::create_dir(root).unwrap();
        case.initialize(
            Integration {
                provider: Provider::Mercurial,
                root: root.clone(),
                executable: case.executable.to_str().unwrap().into(),
                ..Default::default()
            },
            None,
        );
        assert!(Provider::Mercurial.detect(root));
    }
    let sa = settings(&a);
    let mut sb = settings(&b);
    sb.endpoint = a.to_str().unwrap().into();
    sb.hg_update_after_pull = true;
    let file = a.join("value with spaces.txt");
    std::fs::write(&file, "42\n").unwrap();
    let (_, unknown) = case.operation(&a, &sa, Operation::Status, Arguments::default());
    assert!(unknown
        .rows
        .iter()
        .any(|row| row.path.as_deref() == Some(&file)
            && row.fields.get("status").map(String::as_str) == Some("?")));
    case.operation(&a, &sa, Operation::Add, paths(&file));
    case.operation(&a, &sa, Operation::Commit, message("owned first42"));
    let (_, first) = case.operation(&a, &sa, Operation::Log, Arguments::default());
    assert_eq!(first.rows.len(), 1);
    assert_eq!(first.rows[0].label, "owned first42");
    let first_id = first.rows[0].revision.clone().unwrap();
    case.operation(
        &a,
        &sa,
        Operation::Bookmark,
        Arguments {
            name: "owned-bookmark".into(),
            ..Default::default()
        },
    );
    let (_, bookmarks) = case.operation(&a, &sa, Operation::Bookmarks, Arguments::default());
    assert!(bookmarks
        .rows
        .iter()
        .any(
            |row| row.fields.get("bookmark").map(String::as_str) == Some("owned-bookmark")
                && row.revision.as_deref() == Some(&first_id)
        ));
    let (_, incoming) = case.operation(&b, &sb, Operation::Incoming, Arguments::default());
    assert!(incoming
        .rows
        .iter()
        .any(|row| row.revision.as_deref() == Some(&first_id)));
    case.operation(&b, &sb, Operation::Pull, Arguments::default());
    let copy = b.join("value with spaces.txt");
    assert_eq!(std::fs::read_to_string(&copy).unwrap(), "42\n");
    let (_, received) = case.operation(&b, &sb, Operation::Log, Arguments::default());
    assert_eq!(
        received.rows[0].revision.as_deref(),
        Some(first_id.as_str())
    );
    let (no_changes, empty) = case.operation(&b, &sb, Operation::Incoming, Arguments::default());
    assert_eq!(
        no_changes.exit, 1,
        "real Hg no-incoming code is a successful empty result"
    );
    assert!(empty.rows.is_empty());
    std::fs::write(&copy, "43\n").unwrap();
    case.operation(&b, &sb, Operation::Commit, message("owned second43"));
    let (_, outgoing) = case.operation(&b, &sb, Operation::Outgoing, Arguments::default());
    assert_eq!(outgoing.rows.len(), 1);
    assert_eq!(outgoing.rows[0].label, "owned second43");
    let second_id = outgoing.rows[0].revision.clone().unwrap();
    case.operation(&b, &sb, Operation::Push, Arguments::default());
    case.operation(
        &a,
        &sa,
        Operation::Update,
        Arguments {
            revision: second_id.clone(),
            ..Default::default()
        },
    );
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "43\n");
    let (_, latest) = case.operation(&a, &sa, Operation::Log, Arguments::default());
    assert_eq!(latest.rows[0].revision.as_deref(), Some(second_id.as_str()));
    std::fs::write(&file, "44\n").unwrap();
    let (_, diff) = case.operation(&a, &sa, Operation::Diff, paths(&file));
    assert!(diff.raw.contains("-43") && diff.raw.contains("+44"));
    case.operation(&a, &sa, Operation::Revert, paths(&file));
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "43\n");
    let (_, annotation) = case.operation(&a, &sa, Operation::Annotate, paths(&file));
    assert!(!annotation.rows.is_empty());
}

#[test]
#[ignore = "ROOT-admitted genuine svn/svnadmin; fresh file:// repository and working copies"]
fn configured_subversion_checkout_commit_update_properties_and_locks_use_real_provider() {
    let case = Case::configured(Provider::Subversion, "RUSTPY_TEST_SVN");
    let admin = executable("RUSTPY_TEST_SVNADMIN");
    let a = case.root().join("working-a");
    let b = case.root().join("working-b");
    let repository = case.root().join("repository");
    std::fs::create_dir(&a).unwrap();
    case.initialize(
        Integration {
            provider: Provider::Subversion,
            root: a.clone(),
            executable: case.executable.to_str().unwrap().into(),
            create_local: true,
            repository: repository.clone(),
            ..Default::default()
        },
        Some(&admin),
    );
    assert!(Provider::Subversion.detect(&a));
    let sa = settings(&a);
    let sb = settings(&b);
    let file = a.join("value with spaces.txt");
    std::fs::write(&file, "42\n").unwrap();
    let (_, unknown) = case.operation(&a, &sa, Operation::Status, Arguments::default());
    assert!(unknown
        .rows
        .iter()
        .any(|row| row.path.as_deref() == Some(&file) && row.detail == "unversioned"));
    case.operation(&a, &sa, Operation::Add, paths(&file));
    case.operation(&a, &sa, Operation::Commit, message("owned first42"));
    let (_, first) = case.operation(&a, &sa, Operation::Log, paths(&file));
    assert!(first
        .rows
        .iter()
        .any(|row| row.label == "owned first42" && row.revision.as_deref() == Some("1")));
    let repository_url = url::Url::from_directory_path(&repository)
        .unwrap()
        .to_string();
    case.operation(
        case.root(),
        &settings(case.root()),
        Operation::Checkout,
        Arguments {
            source: repository_url,
            destination: b.to_str().unwrap().into(),
            ..Default::default()
        },
    );
    assert!(Provider::Subversion.detect(&b));
    let copy = b.join("value with spaces.txt");
    assert_eq!(std::fs::read_to_string(&copy).unwrap(), "42\n");
    std::fs::write(&copy, "43\n").unwrap();
    case.operation(&b, &sb, Operation::Commit, message("owned second43"));
    case.operation(&a, &sa, Operation::Update, Arguments::default());
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "43\n");
    let (_, history) = case.operation(&a, &sa, Operation::Log, paths(&file));
    assert!(history
        .rows
        .iter()
        .any(|row| row.label == "owned second43" && row.revision.as_deref() == Some("2")));
    let ignores = "*.pyc\n__pycache__";
    // svn:ignore is a special line-list property: SVN canonicalizes its stored
    // bytes with a final newline, even when propset input lacks that newline.
    let stored_ignores = "*.pyc\n__pycache__\n";
    let custom_property = "  leading café & 雪\n\tsecond line  \n\n";
    case.operation(
        &a,
        &sa,
        Operation::Ignore,
        Arguments {
            value: ignores.into(),
            ..paths(&a)
        },
    );
    case.operation(
        &a,
        &sa,
        Operation::SetProperty,
        Arguments {
            name: "rustpy:owned-whitespace".into(),
            value: custom_property.into(),
            ..paths(&a)
        },
    );
    let (_, properties) = case.operation(&a, &sa, Operation::Properties, paths(&a));
    assert!(properties.rows.iter().any(|row| row.label == "svn:ignore"
        && row.fields.get("value").map(String::as_str) == Some(stored_ignores)));
    assert!(properties
        .rows
        .iter()
        .any(|row| row.label == "rustpy:owned-whitespace"
            && row.fields.get("value").map(String::as_str) == Some(custom_property)));
    case.operation(&a, &sa, Operation::Commit, message("owned ignore property"));
    case.operation(&b, &sb, Operation::Update, Arguments::default());
    let (_, copied_properties) = case.operation(&b, &sb, Operation::Properties, paths(&b));
    assert!(copied_properties
        .rows
        .iter()
        .any(|row| row.label == "svn:ignore"
            && row.fields.get("value").map(String::as_str) == Some(stored_ignores)));
    assert!(copied_properties
        .rows
        .iter()
        .any(|row| row.label == "rustpy:owned-whitespace"
            && row.fields.get("value").map(String::as_str) == Some(custom_property)));
    case.operation(
        &a,
        &sa,
        Operation::Lock,
        Arguments {
            message: "owned lock".into(),
            ..paths(&file)
        },
    );
    let (_, locked) = case.operation(&a, &sa, Operation::Status, paths(&file));
    assert!(locked.raw.contains("<lock>") && locked.raw.contains("owned lock"));
    case.operation(&a, &sa, Operation::Unlock, paths(&file));
    let (_, unlocked) = case.operation(&a, &sa, Operation::Status, paths(&file));
    assert!(!unlocked.raw.contains("<lock>"));
    std::fs::write(&file, "44\n").unwrap();
    let (_, diff) = case.operation(&a, &sa, Operation::Diff, paths(&file));
    assert!(diff.raw.contains("-43") && diff.raw.contains("+44"));
    case.operation(&a, &sa, Operation::Revert, paths(&file));
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "43\n");
    case.operation(&a, &sa, Operation::Cleanup, paths(&a));
}

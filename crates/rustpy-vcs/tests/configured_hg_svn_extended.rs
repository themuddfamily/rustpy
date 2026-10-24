//! ROOT-only extended genuine Mercurial/Subversion acceptance, unexecuted by
//! its author. Same explicit provider admission as configured_hg_svn; a separate
//! binary/selector must be bound by ROOT's central ownership/resource runner.
//! MQ and SVN integrate are optional roadmap `?` clauses: the finite operations
//! below exercise their exposed adapters, not every extension/merge option.
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
    dunce::canonicalize(&path).unwrap()
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
            && dunce::canonicalize(Path::new(&plan.executable)).unwrap() == self.executable
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
            let program = dunce::canonicalize(Path::new(&command.executable)).unwrap();
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

fn hg(case: &Case) -> (PathBuf, RootSettings) {
    let root = case.root().join("repository");
    std::fs::create_dir(&root).unwrap();
    case.initialize(
        Integration {
            provider: Provider::Mercurial,
            root: root.clone(),
            executable: case.executable.to_str().unwrap().into(),
            ..Default::default()
        },
        None,
    );
    assert!(Provider::Mercurial.detect(&root));
    let settings = settings(&root);
    (root, settings)
}
fn history(case: &Case, root: &Path, settings: &RootSettings) -> Data {
    case.operation(root, settings, Operation::Log, Arguments::default())
        .1
}
fn hg_commit_file(
    case: &Case,
    root: &Path,
    settings: &RootSettings,
    name: &str,
    contents: &str,
    description: &str,
) -> String {
    let path = root.join(name);
    std::fs::write(&path, contents).unwrap();
    case.operation(root, settings, Operation::Add, paths(&path));
    case.operation(root, settings, Operation::Commit, message(description));
    let log = history(case, root, settings);
    assert_eq!(log.rows[0].label, description);
    log.rows[0].revision.clone().unwrap()
}
fn revision(value: &str) -> Arguments {
    Arguments {
        revision: value.into(),
        ..Default::default()
    }
}
fn hg_parents(data: &Data) -> Vec<String> {
    let values: serde_json::Value = serde_json::from_str(&data.rows[0].fields["parents"]).unwrap();
    values
        .as_array()
        .unwrap()
        .iter()
        .map(|value| value.as_str().unwrap().to_string())
        .collect()
}
#[test]
#[ignore = "ROOT-admitted genuine hg; divergent owned histories, real merge and graft"]
fn configured_mercurial_merge_and_graft_preserve_actual_history_and_parent_identity() {
    let case = Case::configured(Provider::Mercurial, "RUSTPY_TEST_HG");
    let (root, settings) = hg(&case);
    let base = hg_commit_file(
        &case,
        &root,
        &settings,
        "base π.txt",
        "42\n",
        "owned base42",
    );
    case.operation(
        &root,
        &settings,
        Operation::CreateBranch,
        Arguments {
            name: "owned-feature".into(),
            ..Default::default()
        },
    );
    let feature = hg_commit_file(
        &case,
        &root,
        &settings,
        "feature.txt",
        "feature43\n",
        "owned feature43",
    );
    case.operation(&root, &settings, Operation::Update, revision(&base));
    let target = hg_commit_file(
        &case,
        &root,
        &settings,
        "target.txt",
        "target44\n",
        "owned target44",
    );
    case.operation(&root, &settings, Operation::Merge, revision(&feature));
    assert_eq!(
        std::fs::read_to_string(root.join("feature.txt")).unwrap(),
        "feature43\n"
    );
    assert_eq!(
        std::fs::read_to_string(root.join("target.txt")).unwrap(),
        "target44\n"
    );
    case.operation(
        &root,
        &settings,
        Operation::Commit,
        message("owned two-parent merge"),
    );
    let merged = history(&case, &root, &settings);
    let merge_id = merged.rows[0].revision.clone().unwrap();
    let parents = hg_parents(&merged);
    assert_eq!(parents.len(), 2);
    assert!(parents.contains(&feature) && parents.contains(&target));
    case.operation(&root, &settings, Operation::Update, revision(&base));
    case.operation(
        &root,
        &settings,
        Operation::CreateBranch,
        Arguments {
            name: "owned-graft-source".into(),
            ..Default::default()
        },
    );
    let graft_source = hg_commit_file(
        &case,
        &root,
        &settings,
        "graft 雪.txt",
        "grafted45\n",
        "owned graft45",
    );
    case.operation(&root, &settings, Operation::Update, revision(&merge_id));
    case.operation(&root, &settings, Operation::Graft, revision(&graft_source));
    let grafted = history(&case, &root, &settings);
    assert_eq!(grafted.rows[0].label, "owned graft45");
    assert_ne!(
        grafted.rows[0].revision.as_deref(),
        Some(graft_source.as_str())
    );
    let parents = hg_parents(&grafted);
    assert_eq!(parents[0], merge_id);
    assert_eq!(
        std::fs::read_to_string(root.join("graft 雪.txt")).unwrap(),
        "grafted45\n"
    );
    assert_eq!(
        std::fs::read_to_string(root.join("target.txt")).unwrap(),
        "target44\n"
    );
    let status = case
        .operation(&root, &settings, Operation::Status, Arguments::default())
        .1;
    assert!(
        status.rows.is_empty(),
        "merge/graft must leave a committed clean tree"
    );
}
#[test]
#[ignore = "ROOT-admitted genuine hg with bundled rebase/shelve/MQ; actual owned extension state"]
fn configured_mercurial_rebase_shelve_and_optional_mq_change_real_owned_state() {
    let case = Case::configured(Provider::Mercurial, "RUSTPY_TEST_HG");
    let (root, settings) = hg(&case);
    let base = hg_commit_file(&case, &root, &settings, "base.txt", "42\n", "owned base42");
    let left = hg_commit_file(
        &case,
        &root,
        &settings,
        "left.txt",
        "left43\n",
        "owned left43",
    );
    case.operation(&root, &settings, Operation::Update, revision(&base));
    let right = hg_commit_file(
        &case,
        &root,
        &settings,
        "right.txt",
        "right44\n",
        "owned right44",
    );
    case.operation(&root, &settings, Operation::Rebase, revision(&left));
    let rebased = history(&case, &root, &settings);
    assert_eq!(rebased.rows[0].label, "owned right44");
    assert_ne!(rebased.rows[0].revision.as_deref(), Some(right.as_str()));
    assert_eq!(hg_parents(&rebased)[0], left);
    assert!(rebased
        .rows
        .iter()
        .all(|row| row.revision.as_deref() != Some(right.as_str())));
    let file = root.join("left.txt");
    assert_eq!(
        std::fs::read_to_string(root.join("right.txt")).unwrap(),
        "right44\n"
    );
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "left43\n");
    std::fs::write(&file, "shelved π45\n").unwrap();
    let shelf = || Arguments {
        name: "owned-shelf".into(),
        ..Default::default()
    };
    case.operation(&root, &settings, Operation::Shelve, shelf());
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "left43\n");
    assert!(case
        .operation(&root, &settings, Operation::Status, Arguments::default())
        .1
        .rows
        .is_empty());
    case.operation(&root, &settings, Operation::Unshelve, shelf());
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "shelved π45\n");
    case.operation(&root, &settings, Operation::Revert, paths(&file));
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "left43\n");
    // MQ is explicitly optional in the roadmap; this fixture requires the
    // admitted distribution's real extension rather than silently skipping it.
    case.operation(&root, &settings, Operation::QueueInit, Arguments::default());
    std::fs::write(&file, "queued 雪46\n").unwrap();
    case.operation(
        &root,
        &settings,
        Operation::QueueNew,
        Arguments {
            name: "owned.patch".into(),
            message: "owned MQ46".into(),
            ..Default::default()
        },
    );
    let series = case
        .operation(
            &root,
            &settings,
            Operation::QueueSeries,
            Arguments::default(),
        )
        .1;
    assert_eq!(series.raw.trim(), "owned.patch");
    assert_eq!(history(&case, &root, &settings).rows[0].label, "owned MQ46");
    case.operation(&root, &settings, Operation::QueuePop, Arguments::default());
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "left43\n");
    case.operation(
        &root,
        &settings,
        Operation::QueuePush,
        Arguments {
            name: "owned.patch".into(),
            ..Default::default()
        },
    );
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "queued 雪46\n");
    assert_eq!(history(&case, &root, &settings).rows[0].label, "owned MQ46");
    assert!(case
        .operation(&root, &settings, Operation::Status, Arguments::default())
        .1
        .rows
        .is_empty());
}

#[test]
#[ignore = "ROOT-admitted genuine svn/svnadmin; owned import, branch/tag/merge, changelist and relocated repository"]
fn configured_subversion_import_branches_merge_changelists_and_relocate_use_real_provider() {
    let case = Case::configured(Provider::Subversion, "RUSTPY_TEST_SVN");
    let admin = executable("RUSTPY_TEST_SVNADMIN");
    let working = case.root().join("working");
    let repository = case.root().join("repository");
    std::fs::create_dir(&working).unwrap();
    case.initialize(
        Integration {
            provider: Provider::Subversion,
            root: working.clone(),
            executable: case.executable.to_str().unwrap().into(),
            create_local: true,
            repository: repository.clone(),
            ..Default::default()
        },
        Some(&admin),
    );
    assert!(Provider::Subversion.detect(&working));
    let repository_url = url::Url::from_directory_path(&repository)
        .unwrap()
        .to_string();
    let seed = case.root().join("import-layout");
    for directory in ["trunk", "branches", "tags"] {
        std::fs::create_dir_all(seed.join(directory)).unwrap();
    }
    let relative = "value π with spaces.txt";
    std::fs::write(seed.join("trunk").join(relative), "42\n").unwrap();
    case.operation(
        case.root(),
        &settings(case.root()),
        Operation::Import,
        Arguments {
            destination: repository_url.clone(),
            message: "owned import42".into(),
            ..paths(&seed)
        },
    );
    let preferences = settings(&working);
    case.operation(
        &working,
        &preferences,
        Operation::Update,
        Arguments::default(),
    );
    let trunk = working.join("trunk");
    let file = trunk.join(relative);
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "42\n");
    let imported = history(&case, &working, &preferences);
    assert!(imported
        .rows
        .iter()
        .any(|row| row.label == "owned import42" && row.revision.as_deref() == Some("1")));
    let trunk_url = format!("{}trunk", repository_url);
    let branch_url = format!("{}branches/owned-feature", repository_url);
    let tag_url = format!("{}tags/owned-snapshot", repository_url);
    for (destination, description) in [
        (&branch_url, "owned branch copy"),
        (&tag_url, "owned tag copy"),
    ] {
        case.operation(
            &working,
            &preferences,
            Operation::CopyTag,
            Arguments {
                source: trunk_url.clone(),
                destination: destination.clone(),
                message: description.into(),
                ..Default::default()
            },
        );
    }
    for (source, expected) in [
        (format!("{}branches", repository_url), "owned-feature"),
        (format!("{}tags", repository_url), "owned-snapshot"),
    ] {
        let listed = case
            .operation(
                &working,
                &preferences,
                Operation::Branches,
                Arguments {
                    source,
                    ..Default::default()
                },
            )
            .1;
        assert_eq!(listed.rows.len(), 1);
        assert_eq!(listed.rows[0].label, expected);
        assert_eq!(listed.rows[0].fields["kind"], "dir");
    }
    case.operation(
        &working,
        &preferences,
        Operation::Update,
        Arguments::default(),
    );
    let branch_file = working.join("branches/owned-feature").join(relative);
    let tag_file = working.join("tags/owned-snapshot").join(relative);
    assert_eq!(std::fs::read_to_string(&branch_file).unwrap(), "42\n");
    assert_eq!(std::fs::read_to_string(&tag_file).unwrap(), "42\n");
    std::fs::write(&branch_file, "43\n").unwrap();
    case.operation(
        &working,
        &preferences,
        Operation::Commit,
        Arguments {
            message: "owned branch43".into(),
            ..paths(&branch_file)
        },
    );
    case.operation(
        &working,
        &preferences,
        Operation::Update,
        Arguments::default(),
    );
    let trunk_preferences = settings(&trunk);
    // `integrate?` is the exposed SVN merge alias. A real dry-run must not
    // mutate the target; the subsequent Merge performs the actual change.
    case.operation(
        &trunk,
        &trunk_preferences,
        Operation::Integrate,
        Arguments {
            source: branch_url.clone(),
            revision: "3:4".into(),
            dry_run: true,
            ..Default::default()
        },
    );
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "42\n");
    assert!(case
        .operation(
            &trunk,
            &trunk_preferences,
            Operation::Status,
            Arguments::default()
        )
        .1
        .rows
        .is_empty());
    case.operation(
        &trunk,
        &trunk_preferences,
        Operation::Merge,
        Arguments {
            source: branch_url,
            revision: "3:4".into(),
            ..Default::default()
        },
    );
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "43\n");
    assert_eq!(
        std::fs::read_to_string(&tag_file).unwrap(),
        "42\n",
        "tag copy must retain its original tree"
    );
    case.operation(
        &trunk,
        &trunk_preferences,
        Operation::Commit,
        message("owned merge43"),
    );
    let merged = history(&case, &trunk, &trunk_preferences);
    assert!(merged
        .rows
        .iter()
        .any(|row| row.label == "owned merge43" && row.revision.as_deref() == Some("5")));
    let annotation = case
        .operation(
            &trunk,
            &trunk_preferences,
            Operation::Annotate,
            paths(&file),
        )
        .1;
    assert_eq!(annotation.rows.len(), 1);
    assert_eq!(annotation.rows[0].revision.as_deref(), Some("5"));
    assert!(annotation.rows[0].label.ends_with("43"));
    assert!(annotation.rows[0].label.contains("owned-vcs-fixture"));
    std::fs::write(&file, "44\n").unwrap();
    case.operation(
        &trunk,
        &trunk_preferences,
        Operation::AssignChangelist,
        Arguments {
            name: "owned-list".into(),
            ..paths(&file)
        },
    );
    let changelists = case
        .operation(
            &trunk,
            &trunk_preferences,
            Operation::Changelists,
            Arguments::default(),
        )
        .1;
    assert!(changelists
        .rows
        .iter()
        .any(|row| row.path.as_deref() == Some(&file)
            && row.detail == "modified"
            && row.fields.get("changelist").map(String::as_str) == Some("owned-list")));
    case.operation(
        &trunk,
        &trunk_preferences,
        Operation::AssignChangelist,
        paths(&file),
    );
    let unassigned = case
        .operation(
            &trunk,
            &trunk_preferences,
            Operation::Changelists,
            Arguments::default(),
        )
        .1;
    assert!(unassigned
        .rows
        .iter()
        .any(|row| row.path.as_deref() == Some(&file)
            && row.detail == "modified"
            && !row.fields.contains_key("changelist")));
    case.operation(&trunk, &trunk_preferences, Operation::Revert, paths(&file));
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "43\n");
    // Move only this fixture's already-retired local server data. No daemon,
    // system mount, global config or foreign repository participates.
    let relocated = case.root().join("relocated repository 雪");
    std::fs::rename(&repository, &relocated).unwrap();
    let relocated_url = url::Url::from_directory_path(&relocated)
        .unwrap()
        .to_string();
    case.operation(
        &working,
        &preferences,
        Operation::Relocate,
        Arguments {
            source: repository_url.clone(),
            destination: relocated_url.clone(),
            ..Default::default()
        },
    );
    let info = case
        .operation(
            &working,
            &preferences,
            Operation::Info,
            Arguments::default(),
        )
        .1;
    assert_eq!(info.rows.len(), 1);
    assert_eq!(
        info.rows[0].fields["url"].trim_end_matches('/'),
        relocated_url.trim_end_matches('/')
    );
    assert_eq!(
        info.rows[0].fields["root"].trim_end_matches('/'),
        relocated_url.trim_end_matches('/')
    );
    assert!(!repository.exists());
    case.operation(
        &working,
        &preferences,
        Operation::Update,
        Arguments::default(),
    );
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "43\n");
    assert_eq!(std::fs::read_to_string(&tag_file).unwrap(), "42\n");
    assert!(history(&case, &working, &preferences)
        .rows
        .iter()
        .any(|row| row.label == "owned merge43"));
}

//! Optional host adapter for Microsoft's Visual Studio TFVC `tf.exe` dialect.
//! Official contracts: learn.microsoft.com/azure/devops/repos/tfvc/{workspace,
//! workfold,status,history,checkin,get,shelve,unshelve}-command.
//! No executable is bundled/discovered implicitly. Authentication stays in the
//! vendor credential manager; passwords in argv or a fabricated stdin protocol
//! are deliberately unsupported. All commands use the shared retained runner.
use super::{Arguments, CommandPlan, Format, Operation};
use crate::provider::{Integration, Provider, SetupCommand};
use rustpy_settings::other_vcs::RootSettings;
use std::{collections::BTreeMap, path::Path, time::Duration};

#[derive(Clone, Debug)]
pub struct Authority {
    pub collection: String,
    pub workspace: String,
}

/// Query the actual cached workspace selected by the captured local directory,
/// deliberately without /collection (which would query the requested server
/// instead of establishing the authority the later native command will infer).
pub(super) fn preflight(plan: &CommandPlan) -> CommandPlan {
    CommandPlan {
        tfs_authority: None,
        root: plan.root.clone(),
        provider: Provider::Tfs,
        operation: Operation::Info,
        executable: plan.executable.clone(),
        args: vec![
            "workfold".into(),
            plan.root.to_string_lossy().into_owned(),
            "/noprompt".into(),
        ],
        stdin: None,
        format: Format::Text,
        timeout: plan.timeout,
        success_codes: vec![0],
    }
}
pub(super) fn validate_workspace(text: &str, expected: &Authority) -> anyhow::Result<()> {
    let mut collections = Vec::new();
    let mut workspaces = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if let Some(value) = line.strip_prefix("Collection:") {
            collections.push(value.trim());
        }
        if let Some(value) = line.strip_prefix("Workspace:") {
            workspaces.push(value.trim().split(" (").next().unwrap_or_default());
        }
    }
    anyhow::ensure!(collections.len() == 1 && workspaces.len() == 1, "Cannot establish the native TFVC workspace authority from workfold output; expected English Collection/Workspace metadata, no operation was executed");
    let actual = url::Url::parse(collection(collections[0])?)?;
    let desired = url::Url::parse(collection(&expected.collection)?)?;
    anyhow::ensure!(actual.as_str().trim_end_matches('/') == desired.as_str().trim_end_matches('/') && workspaces[0] == expected.workspace, "Native TFVC workspace collection/name differs from the captured mapping; no operation was executed");
    Ok(())
}

fn line<'a>(value: &'a str, label: &str) -> anyhow::Result<&'a str> {
    anyhow::ensure!(
        !value.trim().is_empty() && !value.contains(['\r', '\n', '\0']),
        "Enter a single-line {label}"
    );
    Ok(value)
}
fn collection(value: &str) -> anyhow::Result<&str> {
    let parsed = url::Url::parse(line(value, "HTTPS project collection URL")?)?;
    anyhow::ensure!(
        parsed.scheme() == "https"
            && parsed.host_str().is_some()
            && parsed.username().is_empty()
            && parsed.password().is_none()
            && parsed.query().is_none()
            && parsed.fragment().is_none(),
        "TFVC requires an HTTPS collection URL without embedded credentials/query/fragment"
    );
    Ok(value)
}
fn workspace(value: &str) -> anyhow::Result<&str> {
    line(value, "workspace name")?;
    anyhow::ensure!(
        value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
            && !value.starts_with('-'),
        "Workspace names may contain letters, numbers, dot, dash and underscore"
    );
    Ok(value)
}
fn executable(value: &str) -> anyhow::Result<&str> {
    line(value, "absolute Visual Studio tf.exe path")?;
    anyhow::ensure!(
        Path::new(value).is_absolute(),
        "Configure an explicit absolute tf.exe executable; PATH fallback is disabled"
    );
    Ok(value)
}
fn server_path(value: &str) -> anyhow::Result<&str> {
    line(value, "TFVC server path")?;
    anyhow::ensure!(
        value.starts_with("$/")
            && !value.split('/').any(|part| matches!(part, "." | ".."))
            && !value.contains(';'),
        "Enter a server folder such as $/Project/Main without revision suffixes"
    );
    Ok(value)
}
pub fn integration(value: &Integration) -> anyhow::Result<Vec<SetupCommand>> {
    anyhow::ensure!(
        !value.create_local,
        "TFVC uses an existing Azure DevOps/TFS collection; no local server is created"
    );
    let program = executable(&value.executable)?;
    let endpoint = collection(&value.endpoint)?;
    let name = workspace(&value.workspace)?;
    let source = server_path(&value.depot)?;
    anyhow::ensure!(
        value.root.is_absolute(),
        "Workspace directory must be absolute"
    );
    line(&value.root.to_string_lossy(), "workspace directory")?;
    Ok(vec![
        SetupCommand {
            executable: program.into(),
            cwd: value.root.clone(),
            args: vec![
                "workspace".into(),
                "/new".into(),
                name.into(),
                format!("/collection:{endpoint}"),
                "/location:local".into(),
                "/noprompt".into(),
            ],
            stdin: None,
        },
        SetupCommand {
            executable: program.into(),
            cwd: value.root.clone(),
            args: vec![
                "workfold".into(),
                "/map".into(),
                source.into(),
                value.root.to_string_lossy().into_owned(),
                format!("/workspace:{name}"),
                format!("/collection:{endpoint}"),
                "/noprompt".into(),
            ],
            stdin: None,
        },
    ])
}
pub fn plan(
    root: &Path,
    operation: Operation,
    args: &Arguments,
    settings: &RootSettings,
    program: &str,
) -> anyhow::Result<CommandPlan> {
    use Operation::*;
    anyhow::ensure!(
        Operation::available(Provider::Tfs).contains(&operation),
        "This operation is unavailable for the optional TFVC adapter"
    );
    let program = executable(program)?;
    let endpoint = collection(&settings.endpoint)?;
    anyhow::ensure!(root.is_absolute(), "Captured TFVC root must be absolute");
    let mut format = Format::Text;
    let mut command = match operation {
        Initialize | MapWorkspace => {
            let setup = integration(&Integration {
                provider: Provider::Tfs,
                root: root.into(),
                executable: program.into(),
                endpoint: endpoint.into(),
                workspace: settings.client.clone(),
                depot: args.source.clone(),
                ..Default::default()
            })?;
            setup[usize::from(operation == MapWorkspace)].args.clone()
        }
        Status => {
            format = Format::TfsStatus;
            vec![
                "status".into(),
                "/format:detailed".into(),
                "/recursive".into(),
            ]
        }
        Log => {
            format = Format::TfsHistory;
            vec![
                "history".into(),
                "/format:detailed".into(),
                format!("/stopafter:{}", settings.history_limit.clamp(1, 10000)),
                "/recursive".into(),
            ]
        }
        Info => {
            format = Format::TfsWorkspaces;
            vec![
                "workspaces".into(),
                "/format:xml".into(),
                format!("/collection:{endpoint}"),
                format!(
                    "/owner:{}",
                    if settings.username.is_empty() {
                        "*"
                    } else {
                        line(&settings.username, "workspace owner")?
                    }
                ),
            ]
        }
        Commit => {
            anyhow::ensure!(
                !args.message.trim().is_empty()
                    && !args.message.contains('\0')
                    && !args.message.starts_with('@'),
                "Enter a check-in comment; @comment-file indirection is not supported"
            );
            vec![
                "checkin".into(),
                "/recursive".into(),
                format!("/comment:{}", args.message),
            ]
        }
        Update => {
            let mut result = vec!["get".into(), "/recursive".into()];
            if args.force {
                result.push("/force".into());
            }
            result
        }
        Add => vec!["add".into(), "/recursive".into()],
        Remove => vec!["delete".into(), "/recursive".into()],
        OpenForEdit => vec!["checkout".into(), "/recursive".into()],
        Revert => vec!["undo".into(), "/recursive".into()],
        Diff => {
            format = Format::UnifiedDiff;
            vec!["difference".into(), "/format:unified".into()]
        }
        Shelve => {
            anyhow::ensure!(
                !args.message.contains('\0') && !args.message.starts_with('@'),
                "Shelveset comments cannot use @file indirection"
            );
            vec![
                "shelve".into(),
                "/recursive".into(),
                workspace(&args.name)?.into(),
                format!("/comment:{}", args.message),
            ]
        }
        Unshelve => vec![
            "unshelve".into(),
            "/recursive".into(),
            workspace(&args.name)?.into(),
        ],
        _ => unreachable!(),
    };
    if !matches!(operation, Initialize | MapWorkspace) {
        command.push("/noprompt".into());
        if matches!(operation, Status | Log) {
            command.push(format!("/collection:{endpoint}"));
        }
        if operation == Status && !settings.client.is_empty() {
            command.push(format!("/workspace:{}", workspace(&settings.client)?));
        }
        if operation != Info {
            let targets = if args.paths.is_empty() {
                vec![root.to_path_buf()]
            } else {
                args.paths.clone()
            };
            for path in targets {
                let path = if path.is_absolute() {
                    path
                } else {
                    root.join(path)
                };
                anyhow::ensure!(
                    path.starts_with(root)
                        && !path
                            .components()
                            .any(|part| matches!(part, std::path::Component::ParentDir)),
                    "TFVC target escapes its captured mapping"
                );
                line(&path.to_string_lossy(), "TFVC target")?;
                command.push(path.to_string_lossy().into_owned());
            }
        }
    }
    Ok(CommandPlan {
        tfs_authority: Some(Authority {
            collection: endpoint.into(),
            workspace: if operation == Info && settings.client.is_empty() {
                String::new()
            } else {
                workspace(&settings.client)?.into()
            },
        }),
        root: root.into(),
        provider: Provider::Tfs,
        operation,
        executable: program.into(),
        args: command,
        stdin: None,
        format,
        timeout: Duration::from_secs(settings.timeout_seconds.clamp(10, 3600) as u64),
        success_codes: vec![0],
    })
}

/// Detailed CLI records are localized by the installed vendor client. Recognized
/// English fields become navigable rows; unknown/localized output stays raw in
/// the console instead of inventing paths or revisions.
pub fn decode(root: &Path, format: Format, text: &str) -> anyhow::Result<super::results::Data> {
    use super::results::{Data, Row};
    let mut data = Data {
        raw: text.into(),
        ..Default::default()
    };
    if format == Format::TfsWorkspaces {
        let xml = roxmltree::Document::parse(text)?;
        for node in xml
            .descendants()
            .filter(|node| node.tag_name().name().eq_ignore_ascii_case("workspace"))
        {
            let fields: BTreeMap<String, String> = node
                .attributes()
                .map(|attr| (attr.name().into(), attr.value().into()))
                .collect();
            let name = fields
                .get("name")
                .or_else(|| fields.get("Name"))
                .cloned()
                .unwrap_or_default();
            if !name.is_empty() {
                data.rows.push(Row {
                    id: name.clone(),
                    label: name,
                    fields,
                    ..Default::default()
                });
            }
        }
        return Ok(data);
    }
    let mut fields = BTreeMap::<String, String>::new();
    let mut last = None::<String>;
    let flush = |fields: &mut BTreeMap<String, String>, data: &mut Data| {
        if fields.is_empty() {
            return;
        }
        let revision = fields.get("Changeset").cloned();
        let item = fields
            .get("Local item")
            .or_else(|| fields.get("Local Item"));
        let path = item
            .map(Path::new)
            .filter(|path| path.is_absolute() && path.starts_with(root))
            .map(Path::to_path_buf);
        let label = fields
            .get("Comment")
            .filter(|s| !s.is_empty())
            .map(|s| s.lines().next().unwrap_or("").to_string())
            .or_else(|| item.cloned())
            .or_else(|| revision.clone())
            .unwrap_or_else(|| "TFVC record".into());
        data.rows.push(Row {
            id: revision
                .clone()
                .unwrap_or_else(|| data.rows.len().to_string()),
            label,
            detail: fields.get("User").cloned().unwrap_or_default(),
            path,
            revision,
            fields: std::mem::take(fields),
        });
    };
    for line in text.lines() {
        let content = line.trim_start();
        if content.starts_with("Changeset:")
            || (content.starts_with("Local item:") && fields.contains_key("Local item"))
            || (content.starts_with("Local Item:") && fields.contains_key("Local Item"))
            || line.starts_with("$/")
        {
            flush(&mut fields, &mut data);
            last = None;
        }
        if line.starts_with("$/") {
            fields.insert("Server item".into(), line.into());
            continue;
        }
        if !line.starts_with(char::is_whitespace) || last.as_deref() != Some("Comment") {
            if let Some((name, value)) = content.split_once(':') {
                if [
                    "Changeset",
                    "User",
                    "Date",
                    "Comment",
                    "Items",
                    "Local item",
                    "Local Item",
                    "Server item",
                    "Change",
                    "Workspace",
                    "Type",
                    "Lock",
                ]
                .contains(&name)
                {
                    fields.insert(name.into(), value.trim_start().into());
                    last = Some(name.into());
                    continue;
                }
            }
        }
        if let Some(key) = &last {
            if let Some(value) = fields.get_mut(key) {
                if !value.is_empty() {
                    value.push('\n');
                }
                value.push_str(line.strip_prefix("  ").unwrap_or(line));
            }
        }
    }
    flush(&mut fields, &mut data);
    Ok(data)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn program() -> &'static str {
        if cfg!(windows) {
            "C:\\tools\\tf.exe"
        } else {
            "/tools/tf.exe"
        }
    }
    fn settings() -> RootSettings {
        RootSettings {
            root: std::env::temp_dir().join("rustpy-tfvc-plan"),
            endpoint: "https://dev.azure.com/owned".into(),
            client: "owned-workspace".into(),
            ..Default::default()
        }
    }
    #[test]
    fn workspace_preflight_requires_the_actual_selected_https_collection_and_name() {
        let expected = Authority {
            collection: "https://dev.azure.com/owned".into(),
            workspace: "owned-workspace".into(),
        };
        assert!(validate_workspace("Workspace: owned-workspace (DOMAIN\\owner)\nCollection: https://dev.azure.com/owned/\n$/Project/Main: C:\\work\n", &expected).is_ok());
        for actual in [
            "Workspace: owned-workspace\nCollection: http://dev.azure.com/owned\n",
            "Workspace: owned-workspace\nCollection: https://dev.azure.com/foreign\n",
            "Workspace: preexisting-foreign\nCollection: https://dev.azure.com/owned\n",
            "Workspace: owned-workspace\nCollection: https://dev.azure.com/owned\nCollection: https://dev.azure.com/foreign\n",
            "unrecognized localized metadata\n",
        ] { assert!(validate_workspace(actual, &expected).is_err()); }
        let settings = settings();
        let planned = plan(
            &settings.root,
            Operation::Commit,
            &Arguments {
                message: "owned".into(),
                ..Default::default()
            },
            &settings,
            program(),
        )
        .unwrap();
        let query = preflight(&planned);
        assert_eq!(query.root, settings.root);
        assert_eq!(query.executable, planned.executable);
        assert_eq!(query.args[0], "workfold");
        assert!(
            !query.args.iter().any(|arg| arg.starts_with("/collection:")),
            "preflight must inspect the actual inferred workspace, not query the desired server"
        );
        assert_eq!(planned.tfs_authority.unwrap().workspace, expected.workspace);
    }
    #[test]
    fn setup_and_operations_capture_collection_workspace_and_each_argument() {
        let settings = settings();
        for operation in [Operation::Initialize, Operation::MapWorkspace] {
            let plan = plan(
                &settings.root,
                operation,
                &Arguments {
                    source: "$/Project/Main".into(),
                    ..Default::default()
                },
                &settings,
                program(),
            )
            .unwrap();
            assert_eq!(plan.provider, Provider::Tfs);
            assert!(plan
                .args
                .contains(&"/collection:https://dev.azure.com/owned".into()));
            assert!(plan.args.contains(&"/noprompt".into()));
            assert!(plan.stdin.is_none());
            if operation == Operation::MapWorkspace {
                assert!(plan.args.contains(&"$/Project/Main".into()));
                assert!(plan.args.contains(&"/workspace:owned-workspace".into()));
            }
        }
        let comment = "Check in a file with spaces";
        let change = plan(
            &settings.root,
            Operation::Commit,
            &Arguments {
                message: comment.into(),
                paths: vec!["file with spaces.txt".into()],
                ..Default::default()
            },
            &settings,
            program(),
        )
        .unwrap();
        assert!(change.args.contains(&format!("/comment:{comment}")));
        assert!(change.args.contains(
            &settings
                .root
                .join("file with spaces.txt")
                .to_string_lossy()
                .into_owned()
        ));
        assert!(!change.args.iter().any(|arg| arg.starts_with("/login:")));
        for operation in [Operation::Shelve, Operation::Unshelve] {
            let selected = settings.root.join("only-this-folder");
            let request = plan(
                &settings.root,
                operation,
                &Arguments {
                    name: "owned-shelf".into(),
                    paths: vec![selected.clone()],
                    ..Default::default()
                },
                &settings,
                program(),
            )
            .unwrap();
            assert!(request.args.contains(&"/recursive".into()));
            assert!(
                request
                    .args
                    .contains(&selected.to_string_lossy().into_owned()),
                "shelveset requests must retain selected scope"
            );
        }
        assert!(plan(
            &settings.root,
            Operation::Status,
            &Arguments::default(),
            &settings,
            "tf"
        )
        .is_err());
        let mut unsafe_source = settings.clone();
        unsafe_source.endpoint = "https://user:secret@dev.azure.com/owned".into();
        assert!(plan(
            &settings.root,
            Operation::Status,
            &Arguments::default(),
            &unsafe_source,
            program()
        )
        .is_err());
        assert!(plan(
            &settings.root,
            Operation::Add,
            &Arguments {
                paths: vec!["../outside".into()],
                ..Default::default()
            },
            &settings,
            program()
        )
        .is_err());
    }
    #[test]
    fn detailed_history_and_status_preserve_comments_and_captured_paths() {
        let root = settings().root;
        let history = decode(Path::new("/work"), Format::TfsHistory, "Changeset: 42\nUser: DOMAIN\\owned\nDate: 2026-10-16\nComment:\n  First line\n  café second line\nItems:\n  edit $/Project/Main/file.txt\nChangeset: 41\nUser: DOMAIN\\other\nComment: Previous\n").unwrap();
        assert_eq!(history.rows.len(), 2);
        assert_eq!(history.rows[0].revision.as_deref(), Some("42"));
        assert_eq!(history.rows[0].label, "First line");
        assert_eq!(
            history.rows[0].fields["Comment"],
            "First line\ncafé second line"
        );
        let local = root.join("file with spaces.txt");
        let status = decode(&root, Format::TfsStatus, &format!("$/Project/Main/file with spaces.txt\n  User: DOMAIN\\owned\n  Change: edit\n  Local item: {}\n", local.display())).unwrap();
        assert_eq!(status.rows[0].path.as_deref(), Some(local.as_path()));
        assert_eq!(status.rows[0].fields["Change"], "edit");
        let foreign = decode(
            Path::new("/work"),
            Format::TfsStatus,
            "Local item: /other/file\nChange: edit\n",
        )
        .unwrap();
        assert!(foreign.rows[0].path.is_none());
        let localized = decode(
            Path::new("/work"),
            Format::TfsHistory,
            "localized unrecognized output\n",
        )
        .unwrap();
        assert!(localized.rows.is_empty());
        assert_eq!(localized.raw, "localized unrecognized output\n");
    }
    #[test]
    fn workspace_xml_and_directory_mapping_are_real_provider_records() {
        let data = decode(Path::new("/work"), Format::TfsWorkspaces, r#"<Workspaces><Workspace name="owned" owner="DOMAIN\user"><WorkingFolders><WorkingFolder local="/work" server="$/Project/Main" /></WorkingFolders></Workspace></Workspaces>"#).unwrap();
        assert_eq!(data.rows[0].label, "owned");
        let directory = tempfile::tempdir().unwrap();
        std::fs::create_dir(directory.path().join("$tf")).unwrap();
        assert!(Provider::Tfs.detect(directory.path()));
        let mut mappings = crate::mapping::DirectoryMappings::default();
        mappings.register_provider(directory.path().into(), Provider::Tfs);
        assert_eq!(
            mappings
                .provider_for(&directory.path().join("src/file.txt"))
                .map(|(_, provider)| provider),
            Some(Provider::Tfs)
        );
    }
}

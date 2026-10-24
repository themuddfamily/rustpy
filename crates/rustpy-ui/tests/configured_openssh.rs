//! Real loopback OpenSSH acceptance. Explicitly run this test alone: it starts
//! its own daemon and keys, never uses an existing server or user SSH config.
use std::path::Path;
use std::time::{Duration, Instant};

#[path = "support/configured_openssh.rs"]
mod openssh_support;
use openssh_support::*;

use rustpy_deploy::{exec, SftpTransport, SshTarget, Transport};
use rustpy_project::interpreter_remote::{discover_configured, RemoteCandidate, RemoteKind};
use rustpy_ui::external_tools::{execution_plan, output_source_path, ExecutionPlan, MacroContext};

fn external_run(plan: &ExecutionPlan, root: &Path) -> rustpy_run::RunSession {
    let mut session = rustpy_run::RunSession::spawn(390, &plan.config, None, root);
    let captured = session.process_id().and_then(identity);
    let deadline = Instant::now() + Duration::from_secs(12);
    while session.is_running() && Instant::now() < deadline {
        session.poll();
        std::thread::sleep(Duration::from_millis(5));
    }
    if session.is_running() {
        session.stop();
        let retirement = Instant::now() + Duration::from_secs(5);
        while session.is_running() && Instant::now() < retirement {
            session.poll();
            std::thread::sleep(Duration::from_millis(5));
        }
        panic!(
            "actual SSH external tool exceeded deadline: {:?}",
            session.status
        );
    }
    session.poll();
    if let Some(captured) = captured {
        assert!(
            retired(captured),
            "external tool SSH child physically retired"
        );
    }
    session
}

#[test]
#[ignore = "requires explicit RUSTPY_OPENSSH_ADMITTED=1, absolute SSHD/SSH/SCP/SSH_KEYGEN/PYTHON paths and existing login user; Linux only"]
fn configured_real_openssh_sdk_sftp_host_key_and_owned_retirement() {
    assert!(
        Path::new("/proc/self/stat").is_file(),
        "this acceptance uses Linux /proc physical identity, never silently skips"
    );
    assert_eq!(std::env::var("RUSTPY_OPENSSH_ADMITTED").as_deref(), Ok("1"));
    let sshd = executable("RUSTPY_TEST_SSHD");
    let ssh = executable("RUSTPY_TEST_SSH");
    let scp = executable("RUSTPY_TEST_SCP");
    let keygen = executable("RUSTPY_TEST_SSH_KEYGEN");
    let python = executable("RUSTPY_PROVIDER_PYTHON");
    let user =
        std::env::var("RUSTPY_TEST_SSH_USER").expect("explicit existing local login user required");
    assert!(
        !user.is_empty()
            && user
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "_-".contains(c)),
        "literal login user required"
    );
    let mut fixture = Fixture::start(&sshd, &keygen, &user);
    let mut daemon = fixture.daemon.take().unwrap();
    let root = fixture.directory.path();
    let port = fixture.port;
    let known = fixture.known.clone();
    let client_config = fixture.client_config.clone();
    let log = fixture.log.clone();
    let path = |name: &str| root.join(name).to_string_lossy().into_owned();
    let known_line = |key: &str| fixture.known_line(key);
    let target = SshTarget::new("127.0.0.1", port, &user, Some(root.join("user")));
    let context = rustpy_deploy::transfer::Context {
        options: rustpy_deploy::options::Options {
            ssh_config_file: client_config.to_string_lossy().into_owned(),
            ssh_auth: "key".into(),
            ..Default::default()
        },
        ..Default::default()
    };
    rustpy_deploy::transfer::with_context(context, || {
        let transport = SftpTransport::new(target.clone(), path("remote"))
            .unwrap()
            .with_programs(ssh.to_string_lossy(), scp.to_string_lossy())
            .with_scp_mode(rustpy_deploy::ScpMode::Sftp)
            .with_timeouts(Duration::from_secs(5), Duration::from_secs(10));
        std::fs::create_dir(root.join("remote")).unwrap();
        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            match transport.test_connection() {
                Ok(_) => break,
                Err(error) => {
                    assert!(
                        daemon.owner.poll(&mut daemon.child).unwrap().is_none(),
                        "sshd exited: {}",
                        std::fs::read_to_string(&log).unwrap()
                    );
                    assert!(
                        Instant::now() < deadline,
                        "real sshd readiness/auth failed: {error:#}; {}",
                        std::fs::read_to_string(&log).unwrap()
                    );
                    std::thread::sleep(Duration::from_millis(20));
                }
            }
        }
        // A real host-key mismatch must fail closed, without transferring data
        // or rewriting the isolated known_hosts authority.
        let wrong = known_line("wrong-host.pub");
        std::fs::write(&known, &wrong).unwrap();
        let error = transport
            .test_connection()
            .expect_err("changed host key must refuse authentication");
        assert!(
            format!("{error:#}").to_lowercase().contains("host key"),
            "{error:#}"
        );
        assert_eq!(std::fs::read_to_string(&known).unwrap(), wrong);
        std::fs::write(&known, known_line("host.pub")).unwrap();
        let payload = b"actual SFTP \0 unicode: \xcf\x80\n";
        let local = root.join("source with spaces.bin");
        std::fs::write(&local, payload).unwrap();
        let remote = path("remote/roundtrip with spaces.bin");
        transport.upload(&local, &remote).unwrap();
        assert_eq!(
            transport.scp_mode(),
            rustpy_deploy::ScpMode::Sftp,
            "must exercise real SFTP, not legacy fallback"
        );
        let entries = transport.list(&path("remote")).unwrap();
        assert!(entries
            .iter()
            .any(|entry| entry.name == "roundtrip with spaces.bin"
                && entry.size == payload.len() as u64
                && !entry.is_dir));
        let downloaded = root.join("downloaded.bin");
        transport.download(&remote, &downloaded).unwrap();
        assert_eq!(std::fs::read(downloaded).unwrap(), payload);

        // This adapter passes arguments to REAL SSH with the private config.
        // No command is locally substituted; probe_detailed owns its child.
        let adapter = root.join("remote-python");
        let remote_quote = r#"remote_quote() { printf "'"; printf '%s' "$1" | sed "s/'/'\\\\''/g"; printf "'"; }
"#;
        let script = format!("#!/bin/sh\n{remote_quote}remote=$(remote_quote {})\nfor arg do remote=\"$remote $(remote_quote \"$arg\")\"; done\nexec {} -F {} -- {} \"$remote\"\n", quote(&python.to_string_lossy()), quote(&ssh.to_string_lossy()), quote(&client_config.to_string_lossy()), quote(&target.destination()));
        std::fs::write(&adapter, script).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&adapter, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let candidate = RemoteCandidate {
            kind: RemoteKind::Ssh,
            target: target.destination(),
            python: python.to_string_lossy().into_owned(),
            project_root: root.into(),
            remote_root: None,
            docker: None,
        };
        let rejected = discover_configured(std::slice::from_ref(&candidate), false, |_| {
            panic!("untrusted discovery must not execute transport")
        });
        assert!(rejected[0].is_err());
        let metadata = discover_configured(&[candidate], true, |_| {
            rustpy_project::interpreter::probe_detailed(&adapter, "configured loopback SSH")
                .map_err(|error| error.label())
        })
        .pop()
        .unwrap()
        .unwrap();
        let local_metadata =
            rustpy_project::interpreter::probe_detailed(&python, "configured local reference")
                .unwrap();
        assert_eq!(
            metadata.interpreter.version,
            local_metadata.interpreter.version
        );
        assert_eq!(
            metadata.interpreter.prefix,
            local_metadata.interpreter.prefix
        );
        assert_eq!(metadata.sysconfig, local_metadata.sysconfig);
        assert_eq!(metadata.bits, local_metadata.bits);
        assert_eq!(metadata.interpreter.path, adapter);

        // Actual persisted external-tool plan -> RunSession -> real SSH. The
        // default transfer context here proves the explicit config owns -F.
        let local_project = root.join("local tool project");
        std::fs::create_dir(&local_project).unwrap();
        let local_source = local_project.join("π diagnostic.py");
        std::fs::write(&local_source, "# actual external tool source\n").unwrap();
        let remote_root = path("remote/tool work");
        transport.mkdirs(&remote_root).unwrap();
        let remote_source = format!("{remote_root}/π diagnostic.py");
        transport.upload(&local_source, &remote_source).unwrap();
        let inert = format!("π ' ; $(touch {})", quote(&path("must-not-exist")));
        let context = MacroContext {
            file: Some(local_source.clone()), project_root: Some(local_project.clone()),
            prompt: Some(inert.clone()),
            selected_text: Some("import json,os,sys;print('ACTUAL_EXTERNAL_TOOL='+json.dumps(dict(argv=sys.argv[1:],cwd=os.getcwd(),value=os.environ.get('OWNED_TOOL_VALUE'),home=os.environ.get('HOME'))));print('REMOTE_DIAGNOSTIC '+sys.argv[1]+':7:3:');print('ACTUAL_EXTERNAL_TOOL_STDERR',file=sys.stderr)".into()),
            ..Default::default()
        };
        let mut tool = rustpy_settings::ExternalTool {
            name: "Actual private-config SSH external tool".into(),
            program: python.to_string_lossy().into_owned(),
            arguments: "-I -c $SelectedText$ $FilePath$ $Prompt$".into(),
            working_dir: "$ProjectFileDir$".into(),
            ..Default::default()
        };
        tool.options.inherit_environment = false;
        tool.options
            .environment
            .insert("OWNED_TOOL_VALUE".into(), "$Prompt$".into());
        tool.options.output_filters =
            vec!["^REMOTE_DIAGNOSTIC $FILE_PATH$:$LINE$:$COLUMN$:$".into()];
        tool.options.ssh = Some(rustpy_settings::external_tools::Ssh {
            host: "127.0.0.1".into(),
            port,
            user: user.clone(),
            key_file: Some(root.join("user")),
            ssh_program: ssh.to_string_lossy().into_owned(),
            ssh_config_file: Some(client_config.clone()),
            remote_project_root: remote_root.clone(),
            remote_interpreter: python.to_string_lossy().into_owned(),
            ..Default::default()
        });
        let plan = rustpy_deploy::transfer::with_context(Default::default(), || {
            execution_plan(&tool, &context)
        })
        .unwrap();
        let completed = external_run(&plan, &local_project);
        assert_eq!(
            completed.status,
            rustpy_run::SessionStatus::Exited(0),
            "{}",
            completed.console.text()
        );
        let stdout = completed
            .output
            .iter()
            .filter(|chunk| chunk.kind == rustpy_run::OutputKind::Stdout)
            .map(|chunk| chunk.text.as_str())
            .collect::<String>();
        let stderr = completed
            .output
            .iter()
            .filter(|chunk| chunk.kind == rustpy_run::OutputKind::Stderr)
            .map(|chunk| chunk.text.as_str())
            .collect::<String>();
        let receipt = stdout
            .lines()
            .find_map(|line| line.strip_prefix("ACTUAL_EXTERNAL_TOOL="))
            .expect("actual remote Python must publish receipt");
        let receipt: serde_json::Value = serde_json::from_str(receipt).unwrap();
        assert_eq!(receipt["argv"], serde_json::json!([remote_source, inert]));
        assert_eq!(receipt["cwd"], remote_root);
        assert_eq!(receipt["value"], context.prompt.as_deref().unwrap());
        assert!(
            receipt["home"].is_null(),
            "remote environment inheritance must actually be disabled"
        );
        assert!(stderr.contains("ACTUAL_EXTERNAL_TOOL_STDERR"));
        assert!(
            !root.join("must-not-exist").exists(),
            "macro argv/environment values must never execute shell syntax"
        );
        let filter = regex::Regex::new(&plan.links.filters[0]).unwrap();
        let diagnostic = stdout
            .lines()
            .find_map(|line| filter.captures(line))
            .expect("actual captured output must match configured production filter");
        assert_eq!(&diagnostic["line"], "7");
        assert_eq!(&diagnostic["column"], "3");
        assert_eq!(
            output_source_path(Path::new(&diagnostic["path"]), &plan.links),
            Some(local_source)
        );
        assert_eq!(
            output_source_path(Path::new("/outside/mapping.py"), &plan.links),
            None
        );
        std::fs::write(&known, known_line("wrong-host.pub")).unwrap();
        let rejected = external_run(&plan, &local_project);
        assert!(matches!(rejected.status, rustpy_run::SessionStatus::Exited(code) if code != 0));
        // System output includes the launched command, whose Python source
        // contains this marker even if SSH refuses to run it. Only stdout
        // from this request can establish that the remote program executed.
        let rejected_stdout = rejected
            .output
            .iter()
            .filter(|chunk| chunk.kind == rustpy_run::OutputKind::Stdout)
            .map(|chunk| chunk.text.as_str())
            .collect::<String>();
        assert!(
            !rejected_stdout.contains("ACTUAL_EXTERNAL_TOOL="),
            "changed key must prevent remote tool execution"
        );
        assert!(rejected.console.text().to_lowercase().contains("host key"));
        std::fs::write(&known, known_line("host.pub")).unwrap();

        let marker = root.join("owned-python.pid");
        let source = format!(
            "import os,time;open({},'w').write(str(os.getpid()));time.sleep(60)",
            serde_json::to_string(&marker.to_string_lossy()).unwrap()
        );
        let remote = rustpy_deploy::ssh::remote_sh(&format!(
            "exec {} -I -c {}",
            quote(&python.to_string_lossy()),
            quote(&source)
        ));
        let spec =
            rustpy_deploy::ssh::ssh_command(&target, &remote).with_program(ssh.to_string_lossy());
        let captured_context = rustpy_deploy::transfer::current();
        std::thread::scope(|scope| {
            let client = scope.spawn(move || {
                rustpy_deploy::transfer::with_context(captured_context, || {
                    exec::run(&spec, Duration::from_secs(12))
                })
            });
            let deadline = Instant::now() + Duration::from_secs(5);
            let remote_identity = loop {
                if let Ok(pid) = std::fs::read_to_string(&marker) {
                    if let Ok(pid) = pid.parse::<u32>() {
                        if let Some(captured) = identity(pid) {
                            break captured;
                        }
                    }
                }
                assert!(
                    Instant::now() < deadline,
                    "real authenticated Python never published owned identity"
                );
                std::thread::sleep(Duration::from_millis(10));
            };
            assert!(
                is_owned_descendant(remote_identity, daemon.identity),
                "only this fixture daemon may own retirement target"
            );
            daemon.stop(); // Normal shared supervisor retirement, never signal guessed PIDs.
            let disconnected = client.join().unwrap();
            eprintln!("actual SSH client after owned session retirement: {disconnected:?}");
            assert!(
                matches!(disconnected, Ok(ref output) if !output.success),
                "stopping owned daemon interrupts actual remote child"
            );
            assert!(
                retired(remote_identity),
                "authenticated remote Python physically retired"
            );
        });
    });
    assert!(
        retired(daemon.identity),
        "sshd must be physically gone, not just stop-requested"
    );
}

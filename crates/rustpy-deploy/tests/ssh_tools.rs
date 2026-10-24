//! [`SftpTransport`] (ssh + scp) and [`RsyncTransport`] (ssh + rsync) end to end, without any
//! network: a fake `ssh` script runs the "remote" side on this machine (`sh -c` for commands,
//! `sftp-server` for the SFTP subsystem, `rsync --server` for rsync) while the real `scp` and
//! `rsync` clients do the transfers. This checks the real command lines, quoting and
//! escaping. Parts whose tools are not installed are skipped with a note.
#![cfg(unix)]

mod common;

use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use common::{mtime, read, write, TempDir};
use rustpy_deploy::ssh::scp_mode_from_version;
use rustpy_deploy::{
    plan_server_sync, upload_paths, RsyncTransport, ScpMode, Server, SftpTransport, SshTarget,
    SyncAction, SyncDirection, Transport,
};

const T: i64 = 1_700_000_000;
/// Never resolvable (RFC 2606), should a real ssh ever be reached.
const HOST: &str = "example.invalid";
const SFTP_SERVERS: [&str; 5] = [
    "/usr/lib/openssh/sftp-server",
    "/usr/libexec/openssh/sftp-server",
    "/usr/lib/ssh/sftp-server",
    "/usr/libexec/sftp-server",
    "/usr/lib/sftp-server",
];
/// File names that need quoting or escaping somewhere along the way.
const TRICKY: [&str; 6] = [
    "a b.py",
    "it's \"q\" $HOME.py",
    "x[1].py",
    "star*?.txt",
    "-dash.py",
    "back\\slash.py",
];

fn installed(program: &str) -> bool {
    match Command::new(program).arg("-V").output() {
        Ok(_) => true,
        Err(e) => e.kind() != std::io::ErrorKind::NotFound,
    }
}

/// The scp mode of the installed OpenSSH client (`ssh -V` only prints the version).
fn client_scp_mode() -> Option<ScpMode> {
    let out = Command::new("ssh").arg("-V").output().ok()?;
    Some(scp_mode_from_version(&format!(
        "{} {}",
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&out.stdout)
    )))
}

fn sh_quote(p: &Path) -> String {
    format!("'{}'", p.display().to_string().replace('\'', r"'\''"))
}

/// Writes a fake `ssh` (named with a space, to test the quoting of the program too). Remote
/// commands run in `home`; with `sftp_server` the SFTP subsystem works, without it subsystem
/// requests fail like on a server without SFTP. Every invocation is logged to `ssh.log`.
fn fake_ssh(dir: &Path, home: &Path, sftp_server: Option<&str>) -> String {
    std::fs::create_dir_all(home).unwrap();
    let subsystem = match sftp_server {
        Some(server) => format!("exec {server}"),
        None => "echo 'subsystem request failed on channel 0' >&2; exit 255".to_string(),
    };
    let script = format!(
        r#"#!/bin/sh
# Fake ssh for tests: runs the remote command on this machine; never contacts a host.
printf '%s\n' "$*" >> {log}
subsystem=0
while [ $# -gt 0 ]; do
  case "$1" in
    -V) echo 'OpenSSH_9.9p1 fake' >&2; exit 0 ;;
    --) shift; break ;;
    -[bcDeEFiIJlLmoOpQRSwWB]) shift 2 ;;
    -s) subsystem=1; shift ;;
    -*) shift ;;
    *) break ;;
  esac
done
[ $# -gt 0 ] || {{ echo 'fake ssh: no destination' >&2; exit 255; }}
shift
cd {home} || exit 255
if [ "$subsystem" = 1 ]; then {subsystem}; fi
exec sh -c "$*"
"#,
        log = sh_quote(&dir.join("ssh.log")),
        home = sh_quote(home),
    );
    let path = dir.join("fake ssh");
    std::fs::write(&path, script).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path.to_str().unwrap().to_string()
}

fn all_ok(results: &[(PathBuf, anyhow::Result<()>)]) {
    for (path, result) in results {
        assert!(
            result.is_ok(),
            "{}: {:#}",
            path.display(),
            result.as_ref().unwrap_err()
        );
    }
}

/// Every [`Transport`] operation against the fake server, rooted at `root` (not created yet).
fn exercise(t: &dyn Transport, tmp: &TempDir, root: &Path) {
    let root_s = root.to_str().unwrap();
    let err = t.test_connection().unwrap_err();
    assert!(format!("{err:#}").contains("not accessible"), "{err:#}");
    t.mkdirs(&format!("{root_s}/pkg/[id] dir")).unwrap();
    assert!(root.join("pkg/[id] dir").is_dir());
    let ok = t.test_connection().unwrap();
    assert!(ok.contains(root_s), "{ok}");

    // Uploads keep names and modification times and touch nothing else, even files whose
    // names match a wildcard in the uploaded name.
    write(&root.join("x1.py"), "do not touch", T);
    write(&root.join("starXY.txt"), "do not touch", T);
    let src = tmp.path().join("src");
    for (i, name) in TRICKY.iter().enumerate() {
        let local = src.join(name);
        write(&local, &format!("content of {name}"), T + i as i64);
        t.upload(&local, &format!("{root_s}/{name}"))
            .unwrap_or_else(|e| panic!("upload {name}: {e:#}"));
        assert_eq!(read(&root.join(name)), format!("content of {name}"));
        assert_eq!(mtime(&root.join(name)), T + i as i64, "mtime of {name}");
    }
    let page = src.join("[id].tsx");
    write(&page, "page", T);
    t.upload(&page, &format!("{root_s}/pkg/[id] dir/[id].tsx"))
        .unwrap();
    assert_eq!(read(&root.join("pkg/[id] dir/[id].tsx")), "page");
    assert_eq!(read(&root.join("x1.py")), "do not touch");
    assert_eq!(read(&root.join("starXY.txt")), "do not touch");

    // Listings.
    let entries = t.list(root_s).unwrap();
    let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
    let mut expected: Vec<&str> = TRICKY
        .iter()
        .copied()
        .chain(["pkg", "starXY.txt", "x1.py"])
        .collect();
    expected.sort();
    assert_eq!(names, expected);
    let a = entries.iter().find(|e| e.name == "a b.py").unwrap();
    assert_eq!(
        (a.is_dir, a.size, a.mtime),
        (false, "content of a b.py".len() as u64, Some(T))
    );
    assert!(entries.iter().any(|e| e.name == "pkg" && e.is_dir));
    let all = t.list_recursive(root_s, &["*.txt".to_string()]).unwrap();
    assert!(
        all.iter()
            .any(|e| e.name == "pkg/[id] dir/[id].tsx" && !e.is_dir),
        "{all:?}"
    );
    assert!(
        !all.iter().any(|e| e.name.ends_with(".txt")),
        "excluded names are pruned"
    );
    assert!(t
        .list_recursive(&format!("{root_s}/missing"), &[])
        .unwrap()
        .is_empty());

    // Downloads fetch exactly the named file; a failed one leaves nothing behind.
    for name in TRICKY {
        let local = tmp.path().join("down").join(name);
        t.download(&format!("{root_s}/{name}"), &local)
            .unwrap_or_else(|e| panic!("download {name}: {e:#}"));
        assert_eq!(read(&local), format!("content of {name}"));
    }
    let missing = tmp.path().join("down/missing.py");
    assert!(t
        .download(&format!("{root_s}/missing.py"), &missing)
        .is_err());
    assert!(!missing.exists());

    // A symlinked project file is deployed as a regular file with the link target's content.
    let link = src.join("link.py");
    std::os::unix::fs::symlink(src.join("a b.py"), &link).unwrap();
    t.upload(&link, &format!("{root_s}/link.py")).unwrap();
    assert!(!std::fs::symlink_metadata(root.join("link.py"))
        .unwrap()
        .file_type()
        .is_symlink());
    assert_eq!(read(&root.join("link.py")), "content of a b.py");

    // Deleting.
    t.delete(&format!("{root_s}/pkg")).unwrap();
    t.delete(&format!("{root_s}/it's \"q\" $HOME.py")).unwrap();
    assert!(!root.join("pkg").exists());
    assert!(!root.join("it's \"q\" $HOME.py").exists());
    assert!(root.join("a b.py").exists(), "siblings stay");
    assert!(t.delete("/").is_err());
}

#[test]
fn sftp_transport_end_to_end() {
    let Some(client) = client_scp_mode().filter(|_| installed("scp")) else {
        eprintln!("skipped: the OpenSSH client (ssh, scp) is not installed");
        return;
    };
    let sftp_server = SFTP_SERVERS.into_iter().find(|p| Path::new(p).exists());
    let mode = match (client, sftp_server) {
        (ScpMode::Sftp, Some(_)) => ScpMode::Sftp,
        (ScpMode::Sftp, None) => ScpMode::ForcedLegacy,
        (legacy, _) => legacy,
    };
    let tmp = TempDir::new("sftp");
    let ssh = fake_ssh(tmp.path(), &tmp.path().join("home"), sftp_server);
    let root = tmp.path().join("remote root/it's here");
    let target = SshTarget::new(
        HOST,
        2222,
        "deploy",
        Some(PathBuf::from("/nonexistent/key file")),
    );
    let t = SftpTransport::new(target, root.to_str().unwrap())
        .unwrap()
        .with_programs(ssh, "scp")
        .with_scp_mode(mode)
        .with_timeouts(Duration::from_secs(60), Duration::from_secs(120));
    exercise(&t, &tmp, &root);
    let log = read(&tmp.path().join("ssh.log"));
    assert!(
        log.contains("BatchMode=yes") && log.contains("2222") && log.contains(HOST),
        "{log}"
    );
}

#[test]
fn rsync_transport_end_to_end() {
    if !installed("rsync") {
        eprintln!("skipped: rsync is not installed");
        return;
    }
    let tmp = TempDir::new("rsync");
    let ssh = fake_ssh(tmp.path(), &tmp.path().join("home"), None);
    let root = tmp.path().join("remote root/it's here");
    let t = RsyncTransport::new(
        SshTarget::new(HOST, 2222, "deploy", None),
        root.to_str().unwrap(),
    )
    .unwrap()
    .with_programs(ssh, "rsync")
    .with_timeouts(Duration::from_secs(60), Duration::from_secs(120));
    exercise(&t, &tmp, &root);
    let log = read(&tmp.path().join("ssh.log"));
    assert!(
        log.contains("rsync --server") && log.contains("-p 2222"),
        "{log}"
    );
}

/// The batch API over rsync: remote directories are created as needed (relative to the login
/// directory here), wildcard names land where they belong, and the SSH listing plans a sync.
#[test]
fn upload_paths_and_sync_over_rsync() {
    if !installed("rsync") {
        eprintln!("skipped: rsync is not installed");
        return;
    }
    let tmp = TempDir::new("rsync-batch");
    let home = tmp.path().join("home");
    let ssh = fake_ssh(tmp.path(), &home, None);
    let project = tmp.path().join("project");
    write(&project.join("pages/[id].tsx"), "page", T);
    write(&project.join("pages/[slug]/index.tsx"), "slug", T);
    write(&project.join("my app/main.py"), "main", T);
    write(
        &project.join("my app/__pycache__/main.cpython-312.pyc"),
        "bytecode",
        T,
    );
    // Matches the pattern `[id].tsx`; must survive.
    write(&home.join("site dir/pages/d.tsx"), "remote only", T);

    let server = Server::rsync("r", HOST, "deploy", "~/site dir");
    let t = RsyncTransport::new(SshTarget::new(HOST, 22, "deploy", None), &server.root_path)
        .unwrap()
        .with_programs(ssh, "rsync");
    let mut progress = Vec::new();
    let results = upload_paths(
        &t,
        &server,
        &project,
        std::slice::from_ref(&project),
        |i, n, p| progress.push(format!("{i}/{n} {p}")),
    );
    all_ok(&results);
    assert_eq!(
        progress,
        [
            "0/3 my app/main.py",
            "1/3 pages/[id].tsx",
            "2/3 pages/[slug]/index.tsx"
        ]
    );
    let site = home.join("site dir");
    assert_eq!(read(&site.join("pages/[id].tsx")), "page");
    assert_eq!(read(&site.join("pages/[slug]/index.tsx")), "slug");
    assert_eq!(read(&site.join("my app/main.py")), "main");
    assert_eq!(read(&site.join("pages/d.tsx")), "remote only");
    assert!(!site.join("my app/__pycache__").exists());

    let plan = plan_server_sync(
        &t,
        &server,
        &project,
        &rustpy_deploy::default_excluded(),
        SyncDirection::Both,
    )
    .unwrap();
    let actions: Vec<(&str, SyncAction)> = plan
        .iter()
        .map(|i| (i.rel_path.as_str(), i.action))
        .collect();
    assert_eq!(actions, [("pages/d.tsx", SyncAction::Download)]);
}

#[test]
fn scp_switches_to_the_legacy_protocol_without_an_sftp_subsystem() {
    if client_scp_mode() != Some(ScpMode::Sftp) || !installed("scp") {
        eprintln!("skipped: needs an OpenSSH 9+ client (scp -O)");
        return;
    }
    let tmp = TempDir::new("scp-fallback");
    let ssh = fake_ssh(tmp.path(), &tmp.path().join("home"), None);
    let t = SftpTransport::new(SshTarget::new(HOST, 22, "", None), "/")
        .unwrap()
        .with_programs(ssh, "scp");
    assert_eq!(
        t.scp_mode(),
        ScpMode::Sftp,
        "detected from the fake `ssh -V`"
    );
    let local = tmp.path().join("it's a [test].py");
    write(&local, "fallback", T);
    let remote = tmp.path().join("remote/it's a [test].py");
    std::fs::create_dir_all(remote.parent().unwrap()).unwrap();
    t.upload(&local, remote.to_str().unwrap()).unwrap();
    assert_eq!(read(&remote), "fallback");
    assert_eq!(
        t.scp_mode(),
        ScpMode::ForcedLegacy,
        "remembered for later transfers"
    );
    let back = tmp.path().join("back.py");
    t.download(remote.to_str().unwrap(), &back).unwrap();
    assert_eq!(read(&back), "fallback");
}

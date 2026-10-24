//! Deployment to remote servers (Tools › Deployment, roadmap §6).
//!
//! Transfers shell out to the standard tools instead of linking SSH/FTP libraries:
//!
//! | [`ServerKind`] | transport | tools |
//! |---|---|---|
//! | `Local` | [`LocalTransport`] | file-system copy into a local or mounted folder |
//! | `Sftp`  | [`SftpTransport`]  | `ssh` (list/mkdir/delete) + `scp` (transfers; `scp -O` when the server has no SFTP subsystem) |
//! | `Rsync` | [`RsyncTransport`] | `ssh` + `rsync -az -L -s -e "ssh -p PORT"` |
//! | `Ftp`   | [`FtpTransport`]   | `curl` (`--ftp-create-dirs -T`, `MLSD`/`LIST`, `DELE`/`RMD`, `MFMT`) |
//!
//! Every command line is produced by a pure `*_command` function returning a
//! [`CommandSpec`] (program + args), which [`exec::run`] executes with a timeout; failures
//! become readable errors that include the tool's stderr and a hint. Remote paths are
//! single-quoted inside remote shell scripts, and escaped per tool where the tool itself
//! expands wildcards (see [`ssh`]); the FTP password reaches curl on stdin, never in argv.
//!
//! * [`config`] — [`DeploymentConfig`] stored in `<project>/.rustpy/deployment.json`.
//! * [`paths`] — [`remote_path`] / [`local_path`] mapping (longest mapping prefix wins).
//! * [`sync`] — [`plan_sync`] (size/mtime comparison), [`upload_paths`], [`execute_sync`].
//! * [`glob`] — excluded-path patterns.
//! * [`listing`] — parsers for `find -printf`, `ls -l`, FTP `LIST` and `MLSD` output.
//!
//! Remote paths are POSIX strings, absolute or relative to the login directory (for a local
//! folder: relative to that folder). All operations are blocking; run them on a worker thread.

pub mod config;
pub mod exec;
pub mod ftp;
pub mod glob;
pub mod kubernetes;
pub mod listing;
pub mod local;
pub mod options;
pub mod paths;
mod policy_transport;
pub mod ssh;
pub mod ssh_config;
pub mod ssh_tools;
pub mod sync;
pub mod targets;
pub mod transfer;

#[cfg(test)]
pub(crate) mod test_util;

use std::path::Path;

use serde::{Deserialize, Serialize};

pub use config::{
    default_excluded, AutoUpload, DeploymentConfig, Mapping, Server, ServerKind, DEFAULT_EXCLUDED,
    PLAINTEXT_PASSWORD_WARNING,
};
pub use exec::{sh_quote, CommandSpec, ExecError, Output};
pub use ftp::{FtpTarget, FtpTransport};
pub use glob::{glob_match, is_excluded, ExcludeSet};
pub use listing::{parse_find_output, parse_list_line, parse_ls_output, parse_mlsd_output};
pub use local::LocalTransport;
pub use paths::{
    join_remote, local_path, mapping_local_root, mapping_remote_root, normalize_remote,
    remote_file_name, remote_parent, remote_path, web_url,
};
pub use ssh::{RsyncTransport, ScpMode, SftpTransport, SshTarget};
pub use sync::{
    collect_files, download_paths, execute_sync, plan_server_sync, plan_sync, plan_sync_with,
    upload_paths, SyncAction, SyncDirection, SyncItem, SyncOptions,
};

/// One entry of a remote directory listing.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RemoteEntry {
    /// File name; for recursive listings the `/`-separated path relative to the listed directory.
    pub name: String,
    pub is_dir: bool,
    /// Size in bytes (as reported by the server; meaningless for directories).
    pub size: u64,
    /// Modification time, Unix seconds (UTC), when the server reports one.
    pub mtime: Option<i64>,
}

impl RemoteEntry {
    pub fn file(name: impl Into<String>, size: u64, mtime: Option<i64>) -> Self {
        Self {
            name: name.into(),
            is_dir: false,
            size,
            mtime,
        }
    }

    pub fn dir(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            is_dir: true,
            size: 0,
            mtime: None,
        }
    }
}

/// A way to reach a deployment server. Implementations are cheap to create and thread-safe;
/// every call blocks until the underlying tool finishes (or times out).
pub trait Transport: Send + Sync {
    fn rename(&self, _source: &str, _destination: &str) -> anyhow::Result<()> {
        anyhow::bail!("This transport does not support renaming")
    }
    fn permissions(&self, _path: &str, _mode: u32) -> anyhow::Result<()> {
        anyhow::bail!("This transport does not support permissions")
    }
    /// Copies the local file `local` to the remote file path `remote`, replacing it and keeping
    /// the local modification time where the protocol allows it. The parent directory must
    /// exist unless [`Transport::creates_parent_dirs`] (see [`upload_paths`], which creates it).
    fn upload(&self, local: &Path, remote: &str) -> anyhow::Result<()>;

    /// Copies the remote file `remote` to `local` (parent directories are created; the local
    /// file is only replaced once the transfer succeeded).
    fn download(&self, remote: &str, local: &Path) -> anyhow::Result<()>;

    /// Entries of `remote_dir` (without `.`/`..`), sorted by name.
    fn list(&self, remote_dir: &str) -> anyhow::Result<Vec<RemoteEntry>>;

    /// Deletes a remote file, or a directory with everything inside it; a path that is already
    /// gone is not an error. Deleting `/` or the login directory is refused.
    fn delete(&self, remote: &str) -> anyhow::Result<()>;

    /// Creates `remote_dir` and its missing parents (`mkdir -p`).
    fn mkdirs(&self, remote_dir: &str) -> anyhow::Result<()>;

    /// Connects and checks that the server's root path is accessible; returns a short
    /// human-readable success message.
    fn test_connection(&self) -> anyhow::Result<String>;

    /// Whether listed modification times are coarse (FTP `LIST` gives minutes, or only
    /// days for old files), so sync planning should treat truncated times as equal.
    fn coarse_times(&self) -> bool {
        false
    }

    /// Every entry below `remote_dir` (names are `/`-separated paths relative to it), skipping
    /// excluded paths; empty when `remote_dir` does not exist.
    fn list_recursive(
        &self,
        remote_dir: &str,
        excluded: &[String],
    ) -> anyhow::Result<Vec<RemoteEntry>> {
        walk_recursive(self, remote_dir, excluded)
    }

    /// True when [`Transport::upload`] creates missing parent directories by itself.
    fn creates_parent_dirs(&self) -> bool {
        false
    }

    /// Short description for logs, e.g. `sftp://deploy@example.com:22`.
    fn describe(&self) -> String;
}

/// Directory depth limit of [`walk_recursive`] (guards against symlink loops).
const MAX_WALK_DEPTH: usize = 64;
/// Entry limit of recursive listings.
pub(crate) const MAX_ENTRIES: usize = 500_000;

/// Generic recursive listing built on [`Transport::list`].
pub(crate) fn walk_recursive<T: Transport + ?Sized>(
    transport: &T,
    remote_dir: &str,
    excluded: &[String],
) -> anyhow::Result<Vec<RemoteEntry>> {
    let first = transport.list(remote_dir)?;
    walk_with_root(transport, remote_dir, excluded, first)
}

/// [`walk_recursive`] with the listing of `remote_dir` itself already done.
pub(crate) fn walk_with_root<T: Transport + ?Sized>(
    transport: &T,
    remote_dir: &str,
    excluded: &[String],
    root_entries: Vec<RemoteEntry>,
) -> anyhow::Result<Vec<RemoteEntry>> {
    let set = ExcludeSet::new(excluded);
    let mut out = Vec::new();
    let mut stack: Vec<(String, usize)> = Vec::new();
    let mut next = Some((String::new(), 0usize, root_entries));
    loop {
        let (rel, depth, entries) = match next.take() {
            Some(n) => n,
            None => match stack.pop() {
                Some((rel, depth)) => {
                    let entries = transport.list(&join_remote(remote_dir, &rel))?;
                    (rel, depth, entries)
                }
                None => break,
            },
        };
        for e in entries {
            let child = if rel.is_empty() {
                e.name.clone()
            } else {
                format!("{rel}/{}", e.name)
            };
            if set.is_excluded(&child) {
                continue;
            }
            if e.is_dir && depth < MAX_WALK_DEPTH {
                stack.push((child.clone(), depth + 1));
            }
            out.push(RemoteEntry { name: child, ..e });
            if out.len() > MAX_ENTRIES {
                anyhow::bail!("{} contains more than {MAX_ENTRIES} entries", remote_dir);
            }
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

/// Creates the transport for a configured server.
pub fn transport_for(server: &Server) -> anyhow::Result<Box<dyn Transport>> {
    let inner: Box<dyn Transport> = match &server.kind {
        ServerKind::Local { root } => {
            Box::new(LocalTransport::new(root.clone()).with_root_path(&server.root_path))
        }
        ServerKind::Sftp {
            host,
            port,
            user,
            key_file,
        } => Box::new(SftpTransport::new(
            SshTarget::new(
                if server.options.ssh_alias.is_empty() {
                    host
                } else {
                    &server.options.ssh_alias
                },
                *port,
                user,
                key_file.clone(),
            ),
            &server.root_path,
        )?),
        ServerKind::Rsync { host, user, port } => Box::new(RsyncTransport::new(
            SshTarget::new(
                if server.options.ssh_alias.is_empty() {
                    host
                } else {
                    &server.options.ssh_alias
                },
                *port,
                user,
                None,
            ),
            &server.root_path,
        )?),
        ServerKind::Ftp {
            host,
            port,
            user,
            password,
            ..
        } => Box::new(FtpTransport::new(
            FtpTarget::new(host, *port, user, password.clone()),
            &server.root_path,
        )?),
    };
    Ok(Box::new(policy_transport::PolicyTransport {
        inner,
        options: server.options.clone(),
    }))
}

/// Current time as Unix seconds.
pub(crate) fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Modification time of file metadata as Unix seconds.
pub(crate) fn mtime_secs(meta: &std::fs::Metadata) -> Option<i64> {
    let t = meta.modified().ok()?;
    Some(match t.duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => d.as_secs() as i64,
        Err(e) => -(e.duration().as_secs_f64().ceil() as i64),
    })
}

/// Runs `transfer` against a temporary sibling of `local` and moves it into place on success,
/// so a failed download never clobbers the existing file.
pub(crate) fn download_via_temp(
    local: &Path,
    transfer: impl FnOnce(&Path) -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    use anyhow::Context as _;
    let name = local
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .filter(|n| !n.is_empty())
        .ok_or_else(|| anyhow::anyhow!("invalid local path {}", local.display()))?;
    if let Some(parent) = local.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("cannot create {}", parent.display()))?;
    }
    let tmp = local.with_file_name(format!(".{name}.rustpy-part"));
    let _ = std::fs::remove_file(&tmp);
    match transfer(&tmp) {
        Ok(()) => {
            if !tmp.exists() {
                anyhow::bail!("the transfer did not create {}", local.display());
            }
            std::fs::rename(&tmp, local)
                .with_context(|| format!("cannot replace {}", local.display()))
        }
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    /// In-memory transport: directories map to their entries.
    struct FakeTree {
        dirs: HashMap<String, Vec<RemoteEntry>>,
        listed: Mutex<Vec<String>>,
    }

    impl Transport for FakeTree {
        fn upload(&self, _: &Path, _: &str) -> anyhow::Result<()> {
            unimplemented!()
        }
        fn download(&self, _: &str, _: &Path) -> anyhow::Result<()> {
            unimplemented!()
        }
        fn list(&self, dir: &str) -> anyhow::Result<Vec<RemoteEntry>> {
            self.listed.lock().unwrap().push(dir.to_string());
            self.dirs
                .get(dir)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("no such dir {dir}"))
        }
        fn delete(&self, _: &str) -> anyhow::Result<()> {
            unimplemented!()
        }
        fn mkdirs(&self, _: &str) -> anyhow::Result<()> {
            unimplemented!()
        }
        fn test_connection(&self) -> anyhow::Result<String> {
            Ok("ok".into())
        }
        fn describe(&self) -> String {
            "fake".into()
        }
    }

    #[test]
    fn default_recursive_walk() {
        let mut dirs = HashMap::new();
        dirs.insert(
            "/srv".to_string(),
            vec![
                RemoteEntry::dir("pkg"),
                RemoteEntry::dir(".git"),
                RemoteEntry::file("a.py", 1, Some(5)),
            ],
        );
        dirs.insert(
            "/srv/pkg".to_string(),
            vec![
                RemoteEntry::file("m.py", 2, Some(6)),
                RemoteEntry::file("m.pyc", 2, None),
                RemoteEntry::dir("sub dir"),
            ],
        );
        dirs.insert(
            "/srv/pkg/sub dir".to_string(),
            vec![RemoteEntry::file("x y.txt", 3, None)],
        );
        let fake = FakeTree {
            dirs,
            listed: Mutex::new(Vec::new()),
        };
        let got = fake.list_recursive("/srv", &default_excluded()).unwrap();
        let names: Vec<&str> = got.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "a.py",
                "pkg",
                "pkg/m.py",
                "pkg/sub dir",
                "pkg/sub dir/x y.txt"
            ]
        );
        assert_eq!(got[2], RemoteEntry::file("pkg/m.py", 2, Some(6)));
        assert!(
            !fake
                .listed
                .lock()
                .unwrap()
                .iter()
                .any(|d| d.contains(".git")),
            "excluded dirs are not listed"
        );
        assert!(fake.list_recursive("/missing", &[]).is_err());
        assert!(!fake.creates_parent_dirs());
    }

    #[test]
    fn factory() {
        let t = transport_for(&Server::local("l", "/mnt/x")).unwrap();
        assert_eq!(t.describe(), "file:///mnt/x");
        let t = transport_for(&Server::sftp("s", "example.com", "me", "/srv")).unwrap();
        assert_eq!(t.describe(), "sftp://me@example.com:22");
        let t = transport_for(&Server::rsync("r", "example.com", "", "/srv")).unwrap();
        assert_eq!(t.describe(), "rsync://example.com:22");
        let t = transport_for(&Server::ftp(
            "f",
            "ftp.example.com",
            "bob",
            Some("pw".into()),
            "/",
        ))
        .unwrap();
        assert_eq!(t.describe(), "ftp://bob@ftp.example.com:21");
        assert!(t.creates_parent_dirs());
        assert!(transport_for(&Server::sftp("bad", "-oProxyCommand=x", "me", "/")).is_err());
        assert!(transport_for(&Server::ftp("bad", "a b", "", None, "/")).is_err());
    }
}

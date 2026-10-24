//! Deployment into a local or mounted folder (network share, sshfs/NFS mount, …).

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context as _};

use crate::glob::ExcludeSet;
use crate::paths::{normalize_remote, push_rel};
use crate::{download_via_temp, mtime_secs, RemoteEntry, Transport, MAX_ENTRIES};

/// Copies files into `root`. Remote paths are relative to `root`: `/` (or `.`) is the folder
/// itself and `/pkg/a.py` is `<root>/pkg/a.py`; `..` may not escape it.
#[derive(Clone, Debug)]
pub struct LocalTransport {
    root: PathBuf,
    root_path: String,
}

impl LocalTransport {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            root_path: "/".to_string(),
        }
    }

    /// The server's root path (checked by [`Transport::test_connection`]).
    pub fn with_root_path(mut self, root_path: &str) -> Self {
        self.root_path = root_path.to_string();
        self
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// File-system path of a remote path.
    pub fn resolve(&self, remote: &str) -> anyhow::Result<PathBuf> {
        let norm = normalize_remote(remote);
        if norm == ".." || norm.starts_with("../") {
            bail!("remote path {remote:?} escapes the deployment folder");
        }
        Ok(push_rel(&self.root, norm.trim_start_matches('/')))
    }
}

/// Copies `src` to `dst` through a temporary sibling (atomic replace), keeping the mtime.
fn copy_file(src: &Path, dst: &Path) -> anyhow::Result<()> {
    let meta = fs::metadata(src).with_context(|| format!("cannot read {}", src.display()))?;
    if meta.is_dir() {
        bail!("{} is a directory", src.display());
    }
    if let (Ok(a), Ok(b)) = (fs::canonicalize(src), fs::canonicalize(dst)) {
        if a == b {
            return Ok(()); // the deployment folder is the project itself
        }
    }
    download_via_temp(dst, |tmp| {
        use std::io::{Read, Write};
        let mut source = fs::File::open(src)?;
        let mut destination = fs::File::create(tmp)?;
        let mut buffer = [0u8; 65536];
        let mut copied = 0;
        loop {
            crate::transfer::check()?;
            let count = source.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            destination.write_all(&buffer[..count])?;
            copied += count;
            if let Some(owner) = crate::transfer::current().owner {
                owner.progress(
                    copied,
                    meta.len().min(usize::MAX as u64) as usize,
                    &dst.display().to_string(),
                    "Copying",
                );
            }
        }
        destination.sync_all()?;
        fs::set_permissions(tmp, meta.permissions())?;
        if crate::transfer::current().options.preserve_timestamps {
            if let Ok(mtime) = meta.modified() {
                if let Err(e) = set_file_mtime(tmp, mtime) {
                    log::warn!(
                        "deployment: cannot keep the modification time of {}: {e}",
                        dst.display()
                    );
                }
            }
        }
        Ok(())
    })
}

fn set_file_mtime(path: &Path, mtime: std::time::SystemTime) -> std::io::Result<()> {
    // `futimens` only needs ownership, so a read-only handle works (and read-only files too).
    #[cfg(unix)]
    let file = fs::File::open(path)?;
    #[cfg(not(unix))]
    let file = fs::File::options().write(true).open(path)?;
    file.set_modified(mtime)
}

fn entry_of(name: String, path: &Path) -> Option<RemoteEntry> {
    let meta = fs::metadata(path)
        .or_else(|_| fs::symlink_metadata(path))
        .ok()?;
    Some(RemoteEntry {
        name,
        is_dir: meta.is_dir(),
        size: if meta.is_dir() { 0 } else { meta.len() },
        mtime: mtime_secs(&meta),
    })
}

impl Transport for LocalTransport {
    fn rename(&self, source: &str, destination: &str) -> anyhow::Result<()> {
        anyhow::ensure!(
            !crate::paths::is_root_like(source) && !crate::paths::is_root_like(destination),
            "Cannot rename a deployment root"
        );
        fs::rename(self.resolve(source)?, self.resolve(destination)?)?;
        Ok(())
    }
    fn permissions(&self, path: &str, mode: u32) -> anyhow::Result<()> {
        anyhow::ensure!(mode <= 0o7777, "Invalid permission bits");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(self.resolve(path)?, fs::Permissions::from_mode(mode))?;
        }
        #[cfg(not(unix))]
        {
            let mut permissions = fs::metadata(self.resolve(path)?)?.permissions();
            permissions.set_readonly(mode & 0o222 == 0);
            fs::set_permissions(self.resolve(path)?, permissions)?;
        }
        Ok(())
    }
    fn upload(&self, local: &Path, remote: &str) -> anyhow::Result<()> {
        copy_file(local, &self.resolve(remote)?)
    }

    fn download(&self, remote: &str, local: &Path) -> anyhow::Result<()> {
        copy_file(&self.resolve(remote)?, local)
    }

    fn list(&self, remote_dir: &str) -> anyhow::Result<Vec<RemoteEntry>> {
        let dir = self.resolve(remote_dir)?;
        let rd = fs::read_dir(&dir).with_context(|| format!("cannot list {}", dir.display()))?;
        let mut out: Vec<RemoteEntry> = rd
            .filter_map(Result::ok)
            .filter_map(|de| entry_of(de.file_name().to_string_lossy().into_owned(), &de.path()))
            .collect();
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }

    fn delete(&self, remote: &str) -> anyhow::Result<()> {
        let path = self.resolve(remote)?;
        if path == self.root || normalize_remote(remote) == "/" || normalize_remote(remote) == "." {
            bail!("refusing to delete the deployment folder itself");
        }
        let meta = match fs::symlink_metadata(&path) {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e).with_context(|| format!("cannot delete {}", path.display())),
        };
        if meta.is_dir() {
            fs::remove_dir_all(&path)
        } else {
            fs::remove_file(&path)
        }
        .with_context(|| format!("cannot delete {}", path.display()))
    }

    fn mkdirs(&self, remote_dir: &str) -> anyhow::Result<()> {
        let dir = self.resolve(remote_dir)?;
        fs::create_dir_all(&dir).with_context(|| format!("cannot create {}", dir.display()))
    }

    fn test_connection(&self) -> anyhow::Result<String> {
        if !self.root.is_dir() {
            bail!(
                "folder {} does not exist or is not a directory",
                self.root.display()
            );
        }
        let root = self.resolve(&self.root_path)?;
        if !root.is_dir() {
            bail!(
                "root path {} does not exist in {}",
                self.root_path,
                self.root.display()
            );
        }
        fs::read_dir(&root).with_context(|| format!("cannot read {}", root.display()))?;
        Ok(format!("Folder {} is accessible", root.display()))
    }

    fn list_recursive(
        &self,
        remote_dir: &str,
        excluded: &[String],
    ) -> anyhow::Result<Vec<RemoteEntry>> {
        let base = self.resolve(remote_dir)?;
        if !base.exists() {
            return Ok(Vec::new());
        }
        let set = ExcludeSet::new(excluded);
        let mut out = Vec::new();
        let mut stack = vec![(base, String::new())];
        while let Some((dir, rel)) = stack.pop() {
            let rd =
                fs::read_dir(&dir).with_context(|| format!("cannot list {}", dir.display()))?;
            for de in rd.filter_map(Result::ok) {
                let name = de.file_name().to_string_lossy().into_owned();
                let child = if rel.is_empty() {
                    name
                } else {
                    format!("{rel}/{name}")
                };
                if set.is_excluded(&child) {
                    continue;
                }
                let path = de.path();
                let Some(entry) = entry_of(child.clone(), &path) else {
                    continue;
                };
                let is_link = fs::symlink_metadata(&path)
                    .map(|m| m.file_type().is_symlink())
                    .unwrap_or(false);
                if entry.is_dir && !is_link {
                    stack.push((path, child));
                }
                out.push(entry);
                if out.len() > MAX_ENTRIES {
                    bail!(
                        "{} contains more than {MAX_ENTRIES} entries",
                        self.root.display()
                    );
                }
            }
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }

    fn creates_parent_dirs(&self) -> bool {
        true
    }

    fn describe(&self) -> String {
        format!("file://{}", self.root.display())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::{set_mtime, TempDir};

    #[test]
    fn resolve_stays_inside() {
        let t = LocalTransport::new("/mnt/www");
        assert_eq!(t.resolve("/").unwrap(), PathBuf::from("/mnt/www"));
        assert_eq!(t.resolve(".").unwrap(), PathBuf::from("/mnt/www"));
        assert_eq!(
            t.resolve("/a/b c.py").unwrap(),
            PathBuf::from("/mnt/www/a/b c.py")
        );
        assert_eq!(t.resolve("a/../b").unwrap(), PathBuf::from("/mnt/www/b"));
        assert_eq!(
            t.resolve("/../../etc/passwd").unwrap(),
            PathBuf::from("/mnt/www/etc/passwd")
        );
        assert!(t.resolve("../etc").is_err());
    }

    #[test]
    fn copy_keeps_mtime_and_replaces_atomically() {
        let tmp = TempDir::new("local-copy");
        let src = tmp.write("src/a.txt", "hello");
        set_mtime(&src, 1_600_000_000);
        let t = LocalTransport::new(tmp.path().join("dest"));
        t.upload(&src, "/deep/er/a.txt").unwrap();
        let dst = tmp.path().join("dest/deep/er/a.txt");
        assert_eq!(fs::read_to_string(&dst).unwrap(), "hello");
        assert_eq!(
            mtime_secs(&fs::metadata(&dst).unwrap()),
            Some(1_600_000_000)
        );
        assert!(!tmp.path().join("dest/deep/er/.a.txt.rustpy-part").exists());
        // Uploading a file onto itself is a no-op (not a truncation).
        let same = LocalTransport::new(tmp.path());
        same.upload(&src, "/src/a.txt").unwrap();
        assert_eq!(fs::read_to_string(&src).unwrap(), "hello");
        // Errors are readable.
        let err = t.upload(&tmp.path().join("missing.txt"), "/x").unwrap_err();
        assert!(format!("{err:#}").contains("missing.txt"), "{err:#}");
        assert!(
            t.upload(&tmp.path().join("src"), "/x").is_err(),
            "directories are not files"
        );
        assert!(t
            .download("/nope.txt", &tmp.path().join("got.txt"))
            .is_err());
        assert!(!tmp.path().join("got.txt").exists());
    }

    #[test]
    fn listing_mkdirs_delete_and_connection_test() {
        let tmp = TempDir::new("local-ops");
        let t = LocalTransport::new(tmp.path()).with_root_path("/site");
        let err = t.test_connection().unwrap_err();
        assert!(
            format!("{err:#}").contains("root path /site does not exist"),
            "{err:#}"
        );
        t.mkdirs("/site/pkg/sub dir").unwrap();
        t.mkdirs("site/pkg/sub dir/").unwrap();
        assert!(t.test_connection().unwrap().contains("accessible"));
        assert!(LocalTransport::new(tmp.path().join("missing"))
            .test_connection()
            .is_err());

        let a = tmp.write("site/a.py", "abc");
        set_mtime(&a, 1_650_000_000);
        tmp.write("site/pkg/m.py", "m");
        tmp.write("site/pkg/sub dir/it's \"q\".txt", "q");
        tmp.write("site/pkg/__pycache__/m.cpython-312.pyc", "c");
        tmp.write("site/.git/HEAD", "ref");

        let entries = t.list("/site").unwrap();
        let summary: Vec<(&str, bool, u64)> = entries
            .iter()
            .map(|e| (e.name.as_str(), e.is_dir, e.size))
            .collect();
        assert_eq!(
            summary,
            [(".git", true, 0), ("a.py", false, 3), ("pkg", true, 0)]
        );
        assert_eq!(entries[1].mtime, Some(1_650_000_000));
        assert!(t.list("/site/missing").is_err());
        assert!(t.list("../etc").is_err());

        let all = t
            .list_recursive("/site", &crate::default_excluded())
            .unwrap();
        let names: Vec<&str> = all.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "a.py",
                "pkg",
                "pkg/m.py",
                "pkg/sub dir",
                "pkg/sub dir/it's \"q\".txt"
            ]
        );
        assert!(
            t.list_recursive("/nope", &[]).unwrap().is_empty(),
            "a missing directory lists as empty"
        );

        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(tmp.path().join("site/pkg"), tmp.path().join("site/link"))
                .unwrap();
            let all = t.list_recursive("/site", &[]).unwrap();
            assert!(all.iter().any(|e| e.name == "link" && e.is_dir));
            assert!(
                !all.iter().any(|e| e.name.starts_with("link/")),
                "symlinked directories are not followed"
            );
            t.delete("/site/link").unwrap();
            assert!(
                tmp.path().join("site/pkg/m.py").exists(),
                "deleting a symlink keeps its target"
            );
        }

        t.delete("/site/pkg/sub dir").unwrap();
        assert!(!tmp.path().join("site/pkg/sub dir").exists());
        t.delete("/site/a.py").unwrap();
        assert!(!a.exists());
        t.delete("/site/never-existed.py").unwrap();
        for root_like in ["/", ".", "", "/x/.."] {
            assert!(t.delete(root_like).is_err(), "{root_like:?}");
        }
        assert!(t.delete("../outside").is_err());
        assert!(tmp.path().join("site/pkg/m.py").exists());
        assert_eq!(t.describe(), format!("file://{}", tmp.path().display()));
        assert!(t.creates_parent_dirs());
    }
}

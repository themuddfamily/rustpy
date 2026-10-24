//! Optional immutable pre-save copies. Ordinary saves still use the existing
//! checked writer; backup failure refuses publication, never truncates a file.
use rustpy_core::vfs::{ContentRevision, ReadLimits, Vfs};
use std::{
    io,
    path::{Path, PathBuf},
};

fn digest(bytes: &[u8]) -> String {
    ContentRevision::of(bytes)
        .0
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
/// A stable per-source directory. Canonical aliases use the same identity;
/// non-UTF8 source names cannot collide through lossy display conversion.
pub fn source_directory(root: &Path, path: &Path) -> PathBuf {
    root.join(digest(
        rustpy_core::vfs::normalize_path(path)
            .as_os_str()
            .as_encoded_bytes(),
    ))
}
pub fn backup_path(root: &Path, path: &Path, bytes: &[u8]) -> PathBuf {
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let mut end = name.len().min(120);
    while !name.is_char_boundary(end) {
        end -= 1;
    }
    let name = &name[..end];
    source_directory(root, path).join(format!("{}-{name}.bak", digest(bytes)))
}

fn preserve(
    root: &Path,
    path: &Path,
    bytes: &[u8],
    vfs: &Vfs,
    limits: ReadLimits,
) -> io::Result<PathBuf> {
    use std::io::Write;
    let destination = backup_path(root, path, bytes);
    let parent = destination.parent().unwrap();
    std::fs::create_dir_all(parent)?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    temporary.write_all(bytes)?;
    temporary.as_file().sync_all()?;
    match temporary.persist_noclobber(&destination) {
        Ok(_) => {}
        Err(error) if error.error.kind() == io::ErrorKind::AlreadyExists => {
            if vfs.read(&destination, limits)?.bytes.as_ref() != bytes {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "existing backup does not match its content identity",
                ));
            }
        }
        Err(error) => return Err(error.error),
    }
    Ok(destination)
}

/// Back up only the acknowledged baseline, then recheck it through the original
/// writer. A changed disk file is neither overwritten nor admitted as a backup
/// of the caller's baseline. A race after the final check has the same portable
/// rename limitation as `save_actions::checked_write`.
pub fn checked_write(
    vfs: &Vfs,
    path: &Path,
    bytes: &[u8],
    expected: Option<ContentRevision>,
    limits: ReadLimits,
    backup_root: Option<&Path>,
) -> io::Result<ContentRevision> {
    if let Some(root) = backup_root {
        let baseline = match vfs.read(path, limits) {
            Ok(snapshot) => Some(snapshot),
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => return Err(error),
        };
        if baseline.as_ref().map(|snapshot| snapshot.revision) != expected {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "file changed on disk; reload or Keep before saving",
            ));
        }
        if let Some(snapshot) = baseline.filter(|snapshot| snapshot.bytes.as_ref() != bytes) {
            preserve(root, path, &snapshot.bytes, vfs, limits)?;
        }
    }
    super::save_actions::checked_write(vfs, path, bytes, expected, limits)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exact_original_codec_bytes_survive_save_and_content_versions_are_immutable() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("π.txt");
        let root = dir.path().join("backups");
        let original = b"\xff\xfe\xc0\x03\r\0\n\0";
        std::fs::write(&path, original).unwrap();
        let vfs = Vfs::new();
        let limits = ReadLimits::default();
        let expected = vfs.read(&path, limits).unwrap().revision;
        checked_write(
            &vfs,
            &path,
            b"second\n",
            Some(expected),
            limits,
            Some(&root),
        )
        .unwrap();
        let first = backup_path(&root, &path, original);
        assert_eq!(std::fs::read(&first).unwrap(), original);
        let expected = vfs.read(&path, limits).unwrap().revision;
        checked_write(&vfs, &path, b"third\n", Some(expected), limits, Some(&root)).unwrap();
        assert_eq!(std::fs::read(&first).unwrap(), original);
        assert_eq!(
            std::fs::read(backup_path(&root, &path, b"second\n")).unwrap(),
            b"second\n"
        );
        assert_eq!(std::fs::read(&path).unwrap(), b"third\n");
    }
    #[test]
    fn conflicts_backup_failures_and_new_files_preserve_disk_contract() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("source.txt");
        let root = dir.path().join("backups");
        let vfs = Vfs::new();
        let limits = ReadLimits::default();
        std::fs::write(&path, b"old").unwrap();
        let old = vfs.read(&path, limits).unwrap().revision;
        std::fs::write(&path, b"external").unwrap();
        assert_eq!(
            checked_write(&vfs, &path, b"editor", Some(old), limits, Some(&root))
                .unwrap_err()
                .kind(),
            io::ErrorKind::AlreadyExists
        );
        assert!(!root.exists());
        assert_eq!(std::fs::read(&path).unwrap(), b"external");
        std::fs::write(&root, b"not a directory").unwrap();
        let expected = vfs.read(&path, limits).unwrap().revision;
        assert!(
            checked_write(&vfs, &path, b"editor", Some(expected), limits, Some(&root)).is_err()
        );
        assert_eq!(std::fs::read(&path).unwrap(), b"external");
        let fresh = dir.path().join("new.txt");
        checked_write(&vfs, &fresh, b"new", None, limits, Some(&root)).unwrap();
        assert_eq!(std::fs::read(fresh).unwrap(), b"new");
    }
    #[test]
    fn an_existing_different_backup_is_never_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source.txt");
        let root = dir.path().join("backups");
        std::fs::write(&source, b"original").unwrap();
        let destination = backup_path(&root, &source, b"original");
        std::fs::create_dir_all(destination.parent().unwrap()).unwrap();
        std::fs::write(&destination, b"unrelated").unwrap();
        let vfs = Vfs::new();
        let limits = ReadLimits::default();
        let expected = vfs.read(&source, limits).unwrap().revision;
        assert!(checked_write(
            &vfs,
            &source,
            b"changed",
            Some(expected),
            limits,
            Some(&root)
        )
        .is_err());
        assert_eq!(std::fs::read(source).unwrap(), b"original");
        assert_eq!(std::fs::read(destination).unwrap(), b"unrelated");
    }
    #[test]
    fn long_unicode_source_names_have_bounded_backup_names_and_exact_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(format!("{}.txt", "π".repeat(100)));
        let root = dir.path().join("backups");
        std::fs::write(&path, b"original").unwrap();
        let vfs = Vfs::new();
        let limits = ReadLimits::default();
        let expected = vfs.read(&path, limits).unwrap().revision;
        checked_write(&vfs, &path, b"new", Some(expected), limits, Some(&root)).unwrap();
        let backup = backup_path(&root, &path, b"original");
        assert!(backup.file_name().unwrap().as_encoded_bytes().len() <= 189);
        assert_eq!(std::fs::read(backup).unwrap(), b"original");
        assert_eq!(std::fs::read(path).unwrap(), b"new");
    }
}

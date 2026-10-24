//! Explicit, captured file synchronization. No recursive overwrite or symlink traversal.
use std::path::{Path, PathBuf};
pub(super) struct CopyPlan {
    pub source: PathBuf,
    pub destination: PathBuf,
    source_root: PathBuf,
    destination_root: PathBuf,
    bytes: Vec<u8>,
    expected: Option<Vec<u8>>,
}
fn read(path: &Path) -> Result<Vec<u8>, String> {
    rustpy_core::vfs::Vfs::new()
        .read(
            path,
            rustpy_core::vfs::ReadLimits {
                max_file_bytes: 32 * 1024 * 1024,
                max_cached_bytes: 0,
                max_cached_files: 0,
            },
        )
        .map(|snapshot| snapshot.bytes.to_vec())
        .map_err(|error| error.to_string())
}
fn safe(root: &Path, path: &Path) -> Result<(), String> {
    let relative = path
        .strip_prefix(root)
        .map_err(|_| "File escapes comparison root")?;
    if relative
        .components()
        .any(|component| !matches!(component, std::path::Component::Normal(_)))
    {
        return Err("Invalid relative synchronization path".into());
    }
    let mut current = root.to_path_buf();
    for component in relative.components() {
        current.push(component);
        match std::fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err("Synchronization does not follow symlinks".into())
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.to_string()),
        }
    }
    Ok(())
}
impl CopyPlan {
    pub fn capture(
        source_root: &Path,
        destination_root: &Path,
        relative: &Path,
    ) -> Result<Self, String> {
        let source = source_root.join(relative);
        let destination = destination_root.join(relative);
        safe(source_root, &source)?;
        safe(destination_root, &destination)?;
        let bytes = read(&source)?;
        let expected = if destination.exists() {
            Some(read(&destination)?)
        } else {
            None
        };
        Ok(Self {
            source,
            destination,
            source_root: source_root.into(),
            destination_root: destination_root.into(),
            bytes,
            expected,
        })
    }
    pub fn apply(&self) -> Result<(), String> {
        safe(&self.source_root, &self.source)?;
        safe(&self.destination_root, &self.destination)?;
        if read(&self.source)? != self.bytes {
            return Err("Source changed after synchronization preview".into());
        }
        let current = if self.destination.exists() {
            Some(read(&self.destination)?)
        } else {
            None
        };
        if current != self.expected {
            return Err("Destination changed after synchronization preview".into());
        }
        crate::history_store::snapshot_paths(
            &crate::history_store::base_dir(),
            Some(&self.destination_root),
            std::slice::from_ref(&self.destination),
            "Before directory synchronization",
            false,
        );
        if let Some(parent) = self.destination.parent() {
            std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        crate::editor::save_actions::safe_write(&self.destination, &self.bytes)
            .map_err(|error| error.to_string())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn changed_destination_blocks_copy() {
        let left = tempfile::tempdir().unwrap();
        let right = tempfile::tempdir().unwrap();
        std::fs::write(left.path().join("a"), b"new\0").unwrap();
        std::fs::write(right.path().join("a"), b"old\0").unwrap();
        let plan = CopyPlan::capture(left.path(), right.path(), Path::new("a")).unwrap();
        std::fs::write(right.path().join("a"), b"concurrent").unwrap();
        assert!(plan.apply().is_err());
        assert_eq!(
            std::fs::read(right.path().join("a")).unwrap(),
            b"concurrent"
        );
    }
}

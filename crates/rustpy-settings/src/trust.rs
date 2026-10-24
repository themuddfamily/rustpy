//! IDE-owned trust authority. Missing paths and changed symlinks fail closed.
use std::path::{Path, PathBuf};

/// Runtime identity of the directory opened by this workspace. Persistent trust
/// remains IDE-owned by canonical path; a live workspace must also reject a
/// directory replaced at that path after it was opened.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DirectoryIdentity {
    path: PathBuf,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
    #[cfg(windows)]
    volume: u32,
    #[cfg(windows)]
    file_index: u64,
    #[cfg(not(any(unix, windows)))]
    created: std::time::SystemTime,
}

impl DirectoryIdentity {
    pub fn capture(root: &Path) -> std::io::Result<Self> {
        let path = canonical_directory(root)?;
        if path != root {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "Workspace root is no longer canonical",
            ));
        }
        #[cfg(not(windows))]
        let metadata = std::fs::metadata(&path)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            Ok(Self {
                path,
                device: metadata.dev(),
                inode: metadata.ino(),
            })
        }
        #[cfg(windows)]
        {
            use std::os::windows::{fs::OpenOptionsExt, io::AsRawHandle};
            use windows_sys::Win32::Storage::FileSystem::{
                GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION, FILE_FLAG_BACKUP_SEMANTICS,
                FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
            };
            let directory = std::fs::OpenOptions::new()
                .access_mode(FILE_READ_ATTRIBUTES)
                .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
                .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
                .open(&path)?;
            let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
            if unsafe { GetFileInformationByHandle(directory.as_raw_handle(), &mut info) } == 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(Self {
                path,
                volume: info.dwVolumeSerialNumber,
                file_index: (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow),
            })
        }
        #[cfg(not(any(unix, windows)))]
        {
            Ok(Self {
                path,
                created: metadata.created()?,
            })
        }
    }
    pub fn matches(&self, root: &Path) -> bool {
        Self::capture(root).is_ok_and(|current| current == *self)
    }
}

pub fn canonical_directory(path: &Path) -> std::io::Result<PathBuf> {
    if !path.is_absolute() || !path.is_dir() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "Select an existing absolute directory",
        ));
    }
    dunce::canonicalize(path)
}

/// Stored authorities are canonical paths, never project-controlled aliases.
/// Replacing a trusted directory with a symlink must not transfer its authority.
fn authority(path: &Path) -> Option<PathBuf> {
    let canonical = canonical_directory(path).ok()?;
    (canonical == path).then_some(canonical)
}

pub fn is_trusted(root: &Path, projects: &[PathBuf], locations: &[PathBuf]) -> bool {
    let Ok(root) = canonical_directory(root) else {
        return false;
    };
    projects
        .iter()
        .filter_map(|p| authority(p))
        .any(|p| p == root)
        || locations
            .iter()
            .filter_map(|p| authority(p))
            .any(|p| root.starts_with(p))
}

/// An open workspace is tied to the canonical root captured at open, not a
/// symlink subsequently installed at the same spelling.
pub fn root_unchanged(opened_root: &Path) -> bool {
    authority(opened_root).is_some()
}

#[cfg(test)]
mod fixtures {
    use super::*;
    #[test]
    fn missing_relative_and_component_prefixes_never_grant_trust() {
        let dir = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(dir.path()).unwrap();
        let safe = root.join("safe");
        let sibling = root.join("safe-other");
        std::fs::create_dir(&safe).unwrap();
        std::fs::create_dir(&sibling).unwrap();
        assert!(is_trusted(&safe, &[], std::slice::from_ref(&safe)));
        assert!(!is_trusted(&sibling, &[], std::slice::from_ref(&safe)));
        assert!(!is_trusted(
            &safe.join("missing"),
            &[],
            std::slice::from_ref(&safe)
        ));
        assert!(!is_trusted(&root, &[], &[".".into()]));
    }
    #[test]
    fn replacing_open_directory_at_same_spelling_revokes_live_identity() {
        let dir = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(dir.path()).unwrap();
        let project = root.join("project");
        std::fs::create_dir(&project).unwrap();
        let identity = DirectoryIdentity::capture(&project).unwrap();
        assert!(identity.matches(&project));
        std::fs::rename(&project, root.join("old-project")).unwrap();
        std::fs::create_dir(&project).unwrap();
        assert!(!identity.matches(&project));
    }
    #[cfg(unix)]
    #[test]
    fn symlink_escape_and_replaced_authority_are_rejected() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(dir.path()).unwrap();
        let safe = root.join("safe");
        let outside = root.join("outside");
        std::fs::create_dir(&safe).unwrap();
        std::fs::create_dir(&outside).unwrap();
        let link = safe.join("escape");
        symlink(&outside, &link).unwrap();
        assert!(!is_trusted(&link, &[], std::slice::from_ref(&safe)));
        std::fs::remove_file(link).unwrap();
        std::fs::remove_dir(&safe).unwrap();
        symlink(&outside, &safe).unwrap();
        assert!(!root_unchanged(&safe));
        assert!(!is_trusted(
            &outside,
            std::slice::from_ref(&safe),
            std::slice::from_ref(&safe)
        ));
    }
}

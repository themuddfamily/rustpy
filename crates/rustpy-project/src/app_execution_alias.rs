//! Microsoft Store app-execution aliases (`%LOCALAPPDATA%\Microsoft\WindowsApps\python.exe`).
//!
//! An alias is a zero-length file carrying an `IO_REPARSE_TAG_APPEXECLINK`
//! reparse point. Starting one makes `CreateProcessW` activate the target
//! package first, and that activation can block indefinitely (it waits on the
//! App Execution Alias service). When Python is not installed from the Store
//! the alias belongs to App Installer's "Python redirector", which only opens
//! the Store. So, like the `py` launcher, uv and VS Code's Python extension,
//! discovery never runs a redirector alias, and for a genuine Store Python it
//! resolves the alias to the packaged executable recorded in the reparse data.

use std::path::{Path, PathBuf};

/// `IO_REPARSE_TAG_APPEXECLINK`.
pub const IO_REPARSE_TAG_APPEXECLINK: u32 = 0x8000_001B;

/// The fields of an app-execution link's reparse data.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppExecLink {
    /// Package family name, e.g. `PythonSoftwareFoundation.Python.3.12_qbz5n2kfra8p0`.
    pub package: String,
    /// Application user model ID, e.g. `<package>!Python`.
    pub app_user_model_id: String,
    /// Packaged executable the alias starts.
    pub target: PathBuf,
}

/// What an alias means for interpreter discovery.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StoreAlias {
    /// Python installed from the Store: the packaged executable exists.
    Installed { target: PathBuf },
    /// The App Installer redirector, another package's alias, or a Store
    /// Python whose package is gone. Running it would open the Store or fail.
    NotInstalled { package: String },
}

/// Parses a `REPARSE_DATA_BUFFER` as returned by `FSCTL_GET_REPARSE_POINT`.
/// Returns `None` unless it is a well-formed app-execution link.
pub fn parse_reparse_buffer(buffer: &[u8]) -> Option<AppExecLink> {
    let tag = u32::from_le_bytes(buffer.get(0..4)?.try_into().ok()?);
    if tag != IO_REPARSE_TAG_APPEXECLINK {
        return None;
    }
    let length = u16::from_le_bytes(buffer.get(4..6)?.try_into().ok()?) as usize;
    parse_link_data(buffer.get(8..8 + length)?)
}

/// Parses the tag-specific part: a `u32` version followed by NUL-terminated
/// UTF-16 strings (package, application user model ID, target executable and,
/// from version 3, an application type).
pub fn parse_link_data(data: &[u8]) -> Option<AppExecLink> {
    let version = u32::from_le_bytes(data.get(0..4)?.try_into().ok()?);
    if version == 0 {
        return None;
    }
    let units: Vec<u16> = data[4..]
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| u16::from_le_bytes(*pair))
        .collect();
    let mut strings = units.split(|&unit| unit == 0);
    let mut next = || String::from_utf16(strings.next()?).ok();
    let package = next()?;
    let app_user_model_id = next()?;
    let target = next()?;
    if package.is_empty() || target.is_empty() {
        return None;
    }
    Some(AppExecLink {
        package,
        app_user_model_id,
        target: PathBuf::from(target),
    })
}

/// Builds reparse data for an app-execution link (tests and fixtures).
pub fn reparse_buffer(link: &AppExecLink) -> Vec<u8> {
    let mut data = 3u32.to_le_bytes().to_vec();
    let target = link.target.to_string_lossy();
    for text in [
        link.package.as_str(),
        link.app_user_model_id.as_str(),
        target.as_ref(),
        "0",
    ] {
        for unit in text.encode_utf16().chain([0]) {
            data.extend(unit.to_le_bytes());
        }
    }
    let mut buffer = IO_REPARSE_TAG_APPEXECLINK.to_le_bytes().to_vec();
    buffer.extend((data.len() as u16).to_le_bytes());
    buffer.extend([0, 0]);
    buffer.extend(data);
    buffer
}

/// Decides what an alias stands for. Only a `PythonSoftwareFoundation.Python.*`
/// package whose packaged executable exists counts as an installed interpreter.
pub fn classify(link: &AppExecLink, exists: impl Fn(&Path) -> bool) -> StoreAlias {
    let python_package = link
        .package
        .to_ascii_lowercase()
        .starts_with("pythonsoftwarefoundation.python.");
    if python_package && exists(&link.target) {
        StoreAlias::Installed {
            target: link.target.clone(),
        }
    } else {
        StoreAlias::NotInstalled {
            package: link.package.clone(),
        }
    }
}

/// Reads the app-execution link at `path` without starting it. `None` for any
/// other file (and always off Windows).
pub fn read_link(path: &Path) -> Option<AppExecLink> {
    #[cfg(windows)]
    {
        windows::read_reparse_buffer(path).and_then(|buffer| parse_reparse_buffer(&buffer))
    }
    #[cfg(not(windows))]
    {
        let _ = path;
        None
    }
}

/// Classifies `path` if it is an app-execution alias, without running it.
pub fn store_alias(path: &Path) -> Option<StoreAlias> {
    read_link(path).map(|link| classify(&link, |target| target.is_file()))
}

#[cfg(windows)]
mod windows {
    use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
    use std::os::windows::io::AsRawHandle;
    use std::path::Path;

    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    const FILE_READ_ATTRIBUTES: u32 = 0x80;
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    pub(super) const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    const FSCTL_GET_REPARSE_POINT: u32 = 0x0009_00A8;
    pub(super) const FSCTL_SET_REPARSE_POINT: u32 = 0x0009_00A4;
    const MAXIMUM_REPARSE_DATA_BUFFER_SIZE: usize = 16 * 1024;

    pub(super) fn read_reparse_buffer(path: &Path) -> Option<Vec<u8>> {
        let metadata = std::fs::symlink_metadata(path).ok()?;
        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT == 0 {
            return None;
        }
        let file = std::fs::OpenOptions::new()
            .access_mode(FILE_READ_ATTRIBUTES)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS)
            .open(path)
            .ok()?;
        let mut buffer = vec![0u8; MAXIMUM_REPARSE_DATA_BUFFER_SIZE];
        let mut returned = 0u32;
        // SAFETY: the handle is open for the call; the output buffer and its
        // length match, and no overlapped I/O is requested.
        let ok = unsafe {
            windows_sys::Win32::System::IO::DeviceIoControl(
                file.as_raw_handle(),
                FSCTL_GET_REPARSE_POINT,
                std::ptr::null(),
                0,
                buffer.as_mut_ptr().cast(),
                buffer.len() as u32,
                &mut returned,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            return None;
        }
        buffer.truncate(returned as usize);
        Some(buffer)
    }

    /// Writes app-execution link reparse data onto an existing empty file.
    pub(super) fn set_reparse_buffer(path: &Path, buffer: &[u8]) -> std::io::Result<()> {
        let file = std::fs::OpenOptions::new()
            .write(true)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS)
            .open(path)?;
        let mut returned = 0u32;
        // SAFETY: as in `read_reparse_buffer`; the input buffer is only read.
        let ok = unsafe {
            windows_sys::Win32::System::IO::DeviceIoControl(
                file.as_raw_handle(),
                FSCTL_SET_REPARSE_POINT,
                buffer.as_ptr().cast(),
                buffer.len() as u32,
                std::ptr::null_mut(),
                0,
                &mut returned,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }
}

/// Creates an app-execution alias at `path` (an empty file with the reparse
/// point), for tests. Needs no privilege on NTFS.
#[cfg(windows)]
pub fn create_link_fixture(path: &Path, link: &AppExecLink) -> std::io::Result<()> {
    std::fs::write(path, b"")?;
    windows::set_reparse_buffer(path, &reparse_buffer(link))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store_python() -> AppExecLink {
        AppExecLink {
            package: "PythonSoftwareFoundation.Python.3.12_qbz5n2kfra8p0".into(),
            app_user_model_id: "PythonSoftwareFoundation.Python.3.12_qbz5n2kfra8p0!Python".into(),
            target: PathBuf::from(
                "C:\\Program Files\\WindowsApps\\PythonSoftwareFoundation.Python.3.12_3.12.2800.0_x64__qbz5n2kfra8p0\\python3.12.exe",
            ),
        }
    }

    fn redirector() -> AppExecLink {
        AppExecLink {
            package: "Microsoft.DesktopAppInstaller_8wekyb3d8bbwe".into(),
            app_user_model_id: "Microsoft.DesktopAppInstaller_8wekyb3d8bbwe!PythonRedirector"
                .into(),
            target: PathBuf::from(
                "C:\\Program Files\\WindowsApps\\Microsoft.DesktopAppInstaller_1.21.3482.0_x64__8wekyb3d8bbwe\\AppInstallerPythonRedirector.exe",
            ),
        }
    }

    #[test]
    fn reparse_data_round_trips_and_rejects_other_tags_and_truncation() {
        for link in [store_python(), redirector()] {
            let buffer = reparse_buffer(&link);
            assert_eq!(parse_reparse_buffer(&buffer), Some(link.clone()));
            // Shorter than the declared data length: not a link.
            assert!(parse_reparse_buffer(&buffer[..buffer.len() - 1]).is_none());
        }
        let mut symlink = reparse_buffer(&store_python());
        symlink[0..4].copy_from_slice(&0xA000_000Cu32.to_le_bytes());
        assert!(parse_reparse_buffer(&symlink).is_none());
        assert!(parse_reparse_buffer(&[]).is_none());
        assert!(parse_reparse_buffer(&[0x1B, 0, 0, 0x80, 4, 0, 0, 0, 3, 0, 0, 0]).is_none());
    }

    #[test]
    fn version_one_link_without_application_type_parses() {
        let link = store_python();
        let mut data = 1u32.to_le_bytes().to_vec();
        for text in [
            link.package.clone(),
            link.app_user_model_id.clone(),
            link.target.to_string_lossy().into_owned(),
        ] {
            for unit in text.encode_utf16().chain([0]) {
                data.extend(unit.to_le_bytes());
            }
        }
        assert_eq!(parse_link_data(&data), Some(link));
    }

    #[test]
    fn only_an_existing_store_python_target_is_installed() {
        let python = store_python();
        assert_eq!(
            classify(&python, |_| true),
            StoreAlias::Installed {
                target: python.target.clone()
            }
        );
        assert_eq!(
            classify(&python, |_| false),
            StoreAlias::NotInstalled {
                package: python.package.clone()
            }
        );
        // The redirector only opens the Store, even though its target exists.
        assert!(matches!(
            classify(&redirector(), |_| true),
            StoreAlias::NotInstalled { .. }
        ));
    }

    #[test]
    fn ordinary_files_are_not_aliases() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("python.exe");
        std::fs::write(&file, b"").unwrap();
        assert_eq!(store_alias(&file), None);
        assert_eq!(store_alias(&dir.path().join("missing.exe")), None);
    }

    #[cfg(windows)]
    #[test]
    fn real_reparse_point_fixture_is_detected_and_resolved() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("python3.99.exe");
        std::fs::write(&target, b"MZ").unwrap();
        let mut python = store_python();
        python.target = target.clone();
        let alias = dir.path().join("python.exe");
        if let Err(error) = create_link_fixture(&alias, &python) {
            eprintln!("skipped: cannot create an app-execution link here: {error}");
            return;
        }
        assert_eq!(read_link(&alias), Some(python));
        assert_eq!(store_alias(&alias), Some(StoreAlias::Installed { target }));
        let stub = dir.path().join("python3.exe");
        create_link_fixture(&stub, &redirector()).unwrap();
        assert!(matches!(
            store_alias(&stub),
            Some(StoreAlias::NotInstalled { .. })
        ));
    }
}

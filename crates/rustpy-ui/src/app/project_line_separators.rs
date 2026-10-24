//! Explicit project-wide EditorConfig line separator action; no bulk source rewrite.
use super::*;
use rustpy_text::LineEnding;
use std::io::{self, Read, Write};
use std::sync::atomic::{AtomicU64, Ordering};

struct ConfigLock(std::fs::File);
impl ConfigLock {
    fn acquire(root: &Path) -> io::Result<Self> {
        let dir = root.join(".rustpy");
        std::fs::create_dir_all(&dir)?;
        let directory = std::fs::symlink_metadata(&dir)?;
        if !directory.is_dir() || directory.file_type().is_symlink() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "unsafe project metadata directory",
            ));
        }
        let mut options = std::fs::OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW);
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            options.share_mode(0);
        }
        let file = options.open(dir.join("editorconfig-line-separators.lock"))?;
        if !file.metadata()?.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid configuration lock",
            ));
        }
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
                return Err(io::Error::last_os_error());
            }
        }
        #[cfg(not(any(unix, windows)))]
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "configuration locking unsupported",
        ));
        #[cfg(any(unix, windows))]
        Ok(Self(file))
    }
}
impl Drop for ConfigLock {
    fn drop(&mut self) {
        let _ = &self.0;
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            unsafe {
                libc::flock(self.0.as_raw_fd(), libc::LOCK_UN);
            }
        }
    }
}
struct Existing {
    bytes: Vec<u8>,
    metadata: std::fs::Metadata,
}
fn read_existing(path: &Path) -> io::Result<Option<Existing>> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > 1024 * 1024 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            ".editorconfig must be a regular file under 1 MiB",
        ));
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let mut file = options.open(path)?;
    let opened = file.metadata()?;
    if !same_file(&metadata, &opened) {
        return Err(io::Error::other(".editorconfig changed while opening"));
    }
    let mut bytes = Vec::new();
    (&mut file).take(1024 * 1024 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > 1024 * 1024 || !same_file(&opened, &file.metadata()?) {
        return Err(io::Error::other(".editorconfig changed while reading"));
    }
    std::str::from_utf8(&bytes)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, ".editorconfig is not UTF-8"))?;
    Ok(Some(Existing {
        bytes,
        metadata: opened,
    }))
}
fn same_file(a: &std::fs::Metadata, b: &std::fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if a.dev() != b.dev() || a.ino() != b.ino() {
            return false;
        }
    }
    a.is_file()
        && b.is_file()
        && a.len() == b.len()
        && a.modified().ok() == b.modified().ok()
        && a.permissions() == b.permissions()
}
struct Temporary(PathBuf);
impl Drop for Temporary {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}
fn publish(root: &Path, ending: LineEnding) -> io::Result<()> {
    let root = dunce::canonicalize(root)?;
    let path = root.join(".editorconfig");
    let _lock = ConfigLock::acquire(&root)?;
    let existing = read_existing(&path)?;
    if existing
        .as_ref()
        .is_some_and(|old| old.metadata.permissions().readonly())
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            ".editorconfig is read-only",
        ));
    }
    let mut bytes = existing
        .as_ref()
        .map(|old| old.bytes.clone())
        .unwrap_or_default();
    let separator = if bytes.windows(2).any(|pair| pair == b"\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    if !bytes.is_empty() && !bytes.ends_with(b"\n") && !bytes.ends_with(b"\r") {
        bytes.extend_from_slice(separator.as_bytes());
    }
    let value = match ending {
        LineEnding::Lf => "lf",
        LineEnding::CrLf => "crlf",
        LineEnding::Cr => "cr",
    };
    bytes.extend_from_slice(
        format!("{separator}[*]{separator}end_of_line = {value}{separator}").as_bytes(),
    );
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let temporary_path = root.join(format!(
        ".editorconfig.rustpy-{}-{}.tmp",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary_path)?;
    let temporary = Temporary(temporary_path);
    if let Some(old) = &existing {
        file.set_permissions(old.metadata.permissions())?;
    }
    file.write_all(&bytes)?;
    file.sync_all()?;
    drop(file);
    let current = read_existing(&path)?;
    let unchanged = match (&existing, &current) {
        (None, None) => true,
        (Some(a), Some(b)) => a.bytes == b.bytes && same_file(&a.metadata, &b.metadata),
        _ => false,
    };
    if !unchanged {
        return Err(io::Error::other(".editorconfig changed; retry the action"));
    }
    replace(&temporary.0, &path)?;
    #[cfg(unix)]
    if let Err(error) = std::fs::File::open(&root).and_then(|directory| directory.sync_all()) {
        // Publication has succeeded: never report it as an unchanged failure.
        log::warn!("Project line separators committed but directory sync failed: {error}");
    }
    rustpy_settings::editorconfig::invalidate_cache();
    Ok(())
}
#[cfg(not(windows))]
fn replace(from: &Path, to: &Path) -> io::Result<()> {
    std::fs::rename(from, to)
}
#[cfg(windows)]
fn replace(from: &Path, to: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn MoveFileExW(from: *const u16, to: *const u16, flags: u32) -> i32;
    }
    let from: Vec<_> = from.as_os_str().encode_wide().chain(Some(0)).collect();
    let to: Vec<_> = to.as_os_str().encode_wide().chain(Some(0)).collect();
    if unsafe { MoveFileExW(from.as_ptr(), to.as_ptr(), 1 | 8) } == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}
impl IdeApp {
    pub(super) fn change_project_line_separators(&mut self, ending: LineEnding) {
        let Some(root) = self.project.as_ref().map(|project| project.root.clone()) else {
            return;
        };
        let path = root.join(".editorconfig");
        if self.tabs.iter().any(|tab| {
            tab.state.path.as_deref() == Some(path.as_path()) && tab.state.doc.is_modified()
        }) {
            self.set_status(
                "Save or discard edits to .editorconfig before changing project line separators",
            );
            return;
        }
        match publish(&root,ending) {
            Ok(())=>self.set_status(format!("Project line separators: {} for future saves; nested .editorconfig files retain precedence",ending.label())),
            Err(error)=>self.set_status(format!("Cannot change project line separators: {error}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn project_override_preserves_utf8_crlf_and_existing_pattern_bytes() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join(".editorconfig");
        let original =
            "# π retained\r\nroot = true\r\n[*.py]\r\nindent_size = 3\r\nend_of_line = lf"
                .as_bytes();
        std::fs::write(&path, original).unwrap();
        publish(root.path(), LineEnding::CrLf).unwrap();
        let changed = std::fs::read(&path).unwrap();
        assert!(changed.starts_with(original));
        assert_eq!(
            &changed[original.len()..],
            b"\r\n\r\n[*]\r\nend_of_line = crlf\r\n"
        );
        let ec = rustpy_settings::editorconfig::resolve(&root.path().join("test.py"));
        assert_eq!(ec.end_of_line.as_deref(), Some("crlf"));
        assert_eq!(ec.indent_size, Some(3));
    }
    #[test]
    fn actual_open_buffer_save_uses_updated_cache_and_nested_config_keeps_precedence() {
        let root = tempfile::tempdir().unwrap();
        let file = root.path().join("notes.txt");
        let child = root.path().join("nested");
        std::fs::create_dir(&child).unwrap();
        std::fs::write(
            root.path().join(".editorconfig"),
            "root=true\n[*]\nend_of_line=lf\n",
        )
        .unwrap();
        std::fs::write(child.join(".editorconfig"), "[*]\nend_of_line=cr\n").unwrap();
        let original = "π🙂 one\nsecond\n";
        std::fs::write(&file, original).unwrap();
        let mut app = IdeApp::new(Settings::default(), None);
        app.safe_mode = true;
        app.open_project(root.path());
        app.open_file(&file);
        app.settings.editor.strip_trailing_spaces_on_save = rustpy_settings::StripTrailing::None;
        app.settings.editor.ensure_newline_at_eof = false;
        app.settings.code_style.format_on_save = false;
        assert_eq!(
            rustpy_settings::editorconfig::resolve_cached(&file)
                .end_of_line
                .as_deref(),
            Some("lf")
        );
        for ending in [LineEnding::CrLf, LineEnding::Cr, LineEnding::Lf] {
            let before = std::fs::read(&file).unwrap();
            app.change_project_line_separators(ending);
            assert_eq!(
                std::fs::read(&file).unwrap(),
                before,
                "project preference must not bulk rewrite source"
            );
            assert_eq!(app.tabs[app.active.unwrap()].state.doc.text(), original);
            assert!(app.save_tab(app.active.unwrap()));
            assert_eq!(
                std::fs::read(&file).unwrap(),
                ending.apply(original).as_bytes()
            );
            assert_eq!(
                app.tabs[app.active.unwrap()].state.doc.line_ending(),
                ending
            );
            assert_eq!(
                rustpy_settings::editorconfig::resolve_cached(&child.join("inside.txt"))
                    .end_of_line
                    .as_deref(),
                Some("cr")
            );
        }
    }
    #[cfg(unix)]
    #[test]
    fn readonly_and_symlink_config_refuse_without_changing_bytes_or_cache() {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join(".editorconfig");
        let original = b"root=true\n[*]\nend_of_line=lf\n";
        std::fs::write(&path, original).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o444)).unwrap();
        let file = root.path().join("x.txt");
        assert_eq!(
            rustpy_settings::editorconfig::resolve_cached(&file)
                .end_of_line
                .as_deref(),
            Some("lf")
        );
        assert!(publish(root.path(), LineEnding::CrLf).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), original);
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o444
        );
        assert_eq!(
            rustpy_settings::editorconfig::resolve_cached(&file)
                .end_of_line
                .as_deref(),
            Some("lf")
        );
        std::fs::remove_file(&path).unwrap();
        let external = root.path().join("external");
        std::fs::write(&external, original).unwrap();
        symlink(&external, &path).unwrap();
        assert!(publish(root.path(), LineEnding::Cr).is_err());
        assert_eq!(std::fs::read(external).unwrap(), original);
    }
    #[cfg(unix)]
    #[test]
    fn successful_override_preserves_existing_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join(".editorconfig");
        std::fs::write(&path, "[*]\nindent_style=tab\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).unwrap();
        publish(root.path(), LineEnding::Cr).unwrap();
        assert_eq!(
            std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o640
        );
    }
    #[test]
    fn non_utf8_or_directory_config_is_never_replaced() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join(".editorconfig");
        std::fs::write(&path, [0xff, 0xfe, 0x00]).unwrap();
        assert!(publish(root.path(), LineEnding::Lf).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), [0xff, 0xfe, 0x00]);
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        assert!(publish(root.path(), LineEnding::CrLf).is_err());
        assert!(path.is_dir());
    }
}

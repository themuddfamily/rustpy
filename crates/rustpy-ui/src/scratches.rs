//! Scratch files (roadmap §1.6): quick experiments in any language, kept in
//! the IDE's `<config>/scratches` folder as `scratch.py`, `scratch_1.py`, …,
//! listed under "Scratches and Consoles" in the Project view. They are real
//! files, so they save, run, debug and get inspections like project files.

use std::io;
use std::path::{Path, PathBuf};

/// Languages offered by New Scratch File: (display name, file extension).
pub const LANGUAGES: &[(&str, &str)] = &[
    ("Python", "py"),
    ("Plain text", "txt"),
    ("Markdown", "md"),
    ("JSON", "json"),
    ("SQL", "sql"),
    ("HTTP Request", "http"),
    ("Shell Script", "sh"),
    ("YAML", "yaml"),
    ("TOML", "toml"),
    ("HTML", "html"),
    ("CSS", "css"),
    ("JavaScript", "js"),
    ("INI", "ini"),
    ("reStructuredText", "rst"),
    ("Cython", "pyx"),
    ("Template", "jinja2"),
    ("Gherkin", "feature"),
    ("Dockerfile", "dockerfile"),
    ("Requirements", "requirements"),
];

/// Persist an explicit TextMate grammar identity in an ordinary safe extension.
/// Short scopes are reversible; long scopes use a stable registry-resolved key.
/// Resolution refuses ambiguous keys rather than selecting the wrong grammar.
pub fn textmate_token(scope: &str) -> String {
    if scope.len() <= 80 {
        let hex: String = scope
            .as_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        format!("rustpy-tm-{hex}")
    } else {
        let mut hash: u128 = 0x6c62272e07bb014262b821756295c58d;
        for byte in scope.bytes() {
            hash ^= u128::from(byte);
            hash = hash.wrapping_mul(0x0000000001000000000000000000013b);
        }
        format!("rustpy-tmh-{hash:032x}")
    }
}
pub fn is_textmate_token(ext: &str) -> bool {
    ext.starts_with("rustpy-tm-") || ext.starts_with("rustpy-tmh-")
}

/// The IDE-wide scratches folder (`<config>/scratches`, see `RUSTPY_CONFIG_DIR`).
pub fn scratch_dir() -> PathBuf {
    rustpy_settings::Settings::config_dir().join("scratches")
}

/// Whether `path` is a scratch file (or the scratches folder).
pub fn is_scratch(path: &Path) -> bool {
    let dir = scratch_dir();
    path.starts_with(&dir) || dir.canonicalize().is_ok_and(|d| path.starts_with(d))
}

/// The first free name of `stem.ext`, `stem_1.ext`, `stem_2.ext`, … in `dir`.
fn free_name(dir: &Path, stem: &str, ext: &str) -> PathBuf {
    let file = |n: usize| {
        let base = if n == 0 {
            stem.to_string()
        } else {
            format!("{stem}_{n}")
        };
        if ext.is_empty() {
            base
        } else {
            format!("{base}.{ext}")
        }
    };
    (0..)
        .map(|n| dir.join(file(n)))
        .find(|p| !p.exists())
        .expect("a free scratch name")
}

/// Creates an empty scratch file with extension `ext` in `dir` (created if
/// needed) under the next free `scratch_N` name.
pub fn create(dir: &Path, ext: &str) -> io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    loop {
        let (stem, ext) = if ext == "requirements" {
            ("requirements_scratch", "txt")
        } else {
            ("scratch", ext)
        };
        let path = free_name(dir, stem, ext);
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(_) => return Ok(path),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
}

/// Scratch files in `dir`, sorted by name (missing folder: none).
pub fn list(dir: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| p.is_file())
                .collect()
        })
        .unwrap_or_default();
    files.sort_by_key(|p| p.file_name().map(|n| n.to_string_lossy().to_lowercase()));
    files
}

/// "Change Language": the same name with another extension (a numbered name
/// when that one is taken). Returns the new path.
pub fn change_language(path: &Path, ext: &str) -> io::Result<PathBuf> {
    let is_requirements =
        crate::editor::Language::from_path(path) == crate::editor::Language::Requirements;
    if (ext == "requirements" && is_requirements)
        || (ext != "requirements"
            && !is_requirements
            && path.extension().and_then(|e| e.to_str()) == Some(ext))
    {
        return Ok(path.to_path_buf());
    }
    let dir = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "no folder"))?;
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "scratch".into());
    let stem = stem.strip_prefix("requirements_").unwrap_or(&stem);
    // Legacy requirements*.txt and constraints.txt also carry language identity
    // in their whole filename, not their extension. Escape that identity when
    // leaving Requirements while retaining recognizable source names.
    let ordinary_stem = if is_requirements
        && ext != "requirements"
        && (stem.starts_with("requirements") || stem == "constraints")
    {
        format!("scratch_{stem}")
    } else {
        stem.to_string()
    };
    let stem = ordinary_stem.as_str();
    let (stem, ext) = if ext == "requirements" {
        (format!("requirements_{stem}"), "txt")
    } else {
        (stem.to_string(), ext)
    };
    let target = free_name(dir, &stem, ext);
    std::fs::rename(path, &target)?;
    Ok(target)
}

/// The language name of a scratch file's extension (for menus).
pub fn language_of(path: &Path) -> &'static str {
    if crate::editor::Language::from_path(path) == crate::editor::Language::Requirements {
        return "Requirements";
    }
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    LANGUAGES
        .iter()
        .find(|(_, e)| *e == ext)
        .map(|(name, _)| *name)
        .unwrap_or("Text")
}

/// Five numbered scratch buffers are reused without clearing their contents.
/// Cursor lives outside the scratches listing; renamed language extensions are
/// found by slot stem so a language change does not create a sixth buffer.
pub const BUFFER_COUNT: usize = 5;
pub fn next_buffer(dir: &Path) -> io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let parent = dir.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "scratch directory has no parent",
        )
    })?;
    let _lease = CursorLease::acquire(&parent.join("scratch-buffer-cursor.lock"))?;
    let cursor = parent.join("scratch-buffer-next-slot");
    let slot = match std::fs::read_to_string(&cursor) {
        Ok(value) => value
            .trim()
            .parse::<usize>()
            .ok()
            .filter(|slot| *slot < BUFFER_COUNT)
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidData, "invalid scratch buffer cursor")
            })?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => 0,
        Err(error) => return Err(error),
    };
    let stem = format!("buffer{}", slot + 1);
    let mut matching = list(dir).into_iter().filter(|path| {
        path.file_stem()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.strip_prefix("requirements_").unwrap_or(name) == stem)
    });
    let path = match matching.next() {
        Some(path) => {
            if matching.next().is_some() {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "multiple files occupy scratch buffer slot",
                ));
            }
            path
        }
        None => {
            let path = dir.join(format!("{stem}.txt"));
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)?;
            path
        }
    };
    // This metadata never contains source text. Existing buffer bytes are untouched.
    publish_cursor(&cursor, &format!("{}\n", (slot + 1) % BUFFER_COUNT))?;
    Ok(path)
}

// Separate stable lock inode: replacing the cursor must not release ownership.
struct CursorLease(std::fs::File);
impl CursorLease {
    fn acquire(path: &Path) -> io::Result<Self> {
        let mut options = std::fs::OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            options.share_mode(0);
        }
        let file = options.open(path)?;
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
            "scratch cursor locking unsupported",
        ));
        #[cfg(any(unix, windows))]
        Ok(Self(file))
    }
}
impl Drop for CursorLease {
    fn drop(&mut self) {
        let _ = &self.0; // Windows releases the exclusive handle when this field closes.
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            unsafe {
                libc::flock(self.0.as_raw_fd(), libc::LOCK_UN);
            }
        }
    }
}
fn publish_cursor(path: &Path, value: &str) -> io::Result<()> {
    use std::io::Write;
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let temp = path.with_extension(format!(
        "{}-{}.tmp",
        std::process::id(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)?;
    let result = (|| {
        file.write_all(value.as_bytes())?;
        file.sync_all()?;
        drop(file);
        replace_cursor(&temp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result
}
#[cfg(not(windows))]
fn replace_cursor(from: &Path, to: &Path) -> io::Result<()> {
    std::fs::rename(from, to)
}
#[cfg(windows)]
fn replace_cursor(from: &Path, to: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn MoveFileExW(from: *const u16, to: *const u16, flags: u32) -> i32;
    }
    let from: Vec<u16> = from.as_os_str().encode_wide().chain(Some(0)).collect();
    let to: Vec<u16> = to.as_os_str().encode_wide().chain(Some(0)).collect();
    if unsafe { MoveFileExW(from.as_ptr(), to.as_ptr(), 1 | 8) } == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("rustpy-scratches-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn numbered_names_and_listing() {
        let dir = temp_dir("names");
        let a = create(&dir, "py").unwrap();
        let b = create(&dir, "py").unwrap();
        let c = create(&dir, "md").unwrap();
        assert_eq!(a.file_name().unwrap(), "scratch.py");
        assert_eq!(b.file_name().unwrap(), "scratch_1.py");
        assert_eq!(c.file_name().unwrap(), "scratch.md");
        std::fs::remove_file(&a).unwrap();
        assert_eq!(
            create(&dir, "py").unwrap().file_name().unwrap(),
            "scratch.py",
            "freed names are reused"
        );
        let names: Vec<String> = list(&dir)
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["scratch.md", "scratch.py", "scratch_1.py"]);
        assert!(list(&dir.join("missing")).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn change_language_renames_the_extension() {
        let dir = temp_dir("lang");
        let py = create(&dir, "py").unwrap();
        std::fs::write(&py, "print(1)\n").unwrap();
        let sql = change_language(&py, "sql").unwrap();
        assert_eq!(sql.file_name().unwrap(), "scratch.sql");
        assert_eq!(std::fs::read_to_string(&sql).unwrap(), "print(1)\n");
        assert!(!py.exists());
        assert_eq!(language_of(&sql), "SQL");
        // The target name is taken: the next free one is used.
        let other = create(&dir, "py").unwrap();
        assert_eq!(other.file_name().unwrap(), "scratch.py");
        let again = change_language(&sql, "py").unwrap();
        assert_eq!(again.file_name().unwrap(), "scratch_1.py");
        assert_eq!(
            change_language(&again, "py").unwrap(),
            again,
            "same language: unchanged"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
    #[test]
    fn picker_files_reopen_as_every_builtin_language() {
        use crate::editor::Language;
        let root = tempfile::tempdir().unwrap();
        let expected = [
            Language::Python,
            Language::PlainText,
            Language::Markdown,
            Language::Json,
            Language::Sql,
            Language::Http,
            Language::Shell,
            Language::Yaml,
            Language::Toml,
            Language::Html,
            Language::Css,
            Language::JavaScript,
            Language::Ini,
            Language::Rst,
            Language::Cython,
            Language::Template,
            Language::Gherkin,
            Language::Dockerfile,
            Language::Requirements,
        ];
        for ((_, ext), language) in LANGUAGES.iter().zip(expected) {
            let path = create(root.path(), ext).unwrap();
            assert_eq!(Language::from_path(&path), language, "{ext}");
        }
        let buffer = next_buffer(&root.path().join("scratches")).unwrap();
        let renamed = change_language(&buffer, "requirements").unwrap();
        assert_eq!(Language::from_path(&renamed), Language::Requirements);
        for _ in 1..BUFFER_COUNT {
            next_buffer(&root.path().join("scratches")).unwrap();
        }
        assert_eq!(
            next_buffer(&root.path().join("scratches")).unwrap(),
            renamed
        );
    }

    #[test]
    fn legacy_requirements_filename_can_really_become_plain_text_and_back() {
        use crate::editor::Language;
        let root = tempfile::tempdir().unwrap();
        let original = root.path().join("requirements.txt");
        std::fs::write(&original, "requests==2.32.0\n").unwrap();
        assert_eq!(Language::from_path(&original), Language::Requirements);
        let plain = change_language(&original, "txt").unwrap();
        assert_eq!(plain.file_name().unwrap(), "scratch_requirements.txt");
        assert_eq!(Language::from_path(&plain), Language::PlainText);
        assert!(!original.exists());
        assert_eq!(change_language(&plain, "txt").unwrap(), plain);
        let requirements = change_language(&plain, "requirements").unwrap();
        assert_eq!(Language::from_path(&requirements), Language::Requirements);
        assert_eq!(
            change_language(&requirements, "requirements").unwrap(),
            requirements
        );
        assert_eq!(
            std::fs::read_to_string(&requirements).unwrap(),
            "requests==2.32.0\n"
        );
        assert_eq!(list(root.path()), [requirements]);
    }

    #[test]
    fn requirements_buffer_language_is_idempotent_and_round_trips_to_plain_text() {
        use crate::editor::Language;
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("scratches");
        let plain = next_buffer(&dir).unwrap();
        std::fs::write(&plain, "π preserved\n").unwrap();
        let requirements = change_language(&plain, "requirements").unwrap();
        assert_eq!(
            requirements.file_name().unwrap(),
            "requirements_buffer1.txt"
        );
        assert_eq!(
            change_language(&requirements, "requirements").unwrap(),
            requirements
        );
        assert_eq!(list(&dir).as_slice(), std::slice::from_ref(&requirements));
        let restored = change_language(&requirements, "txt").unwrap();
        assert_eq!(restored, plain);
        assert_eq!(Language::from_path(&restored), Language::PlainText);
        assert_eq!(std::fs::read_to_string(&restored).unwrap(), "π preserved\n");
        assert_eq!(change_language(&restored, "txt").unwrap(), restored);
        for _ in 1..BUFFER_COUNT {
            next_buffer(&dir).unwrap();
        }
        assert_eq!(next_buffer(&dir).unwrap(), restored);
        assert_eq!(list(&dir).len(), BUFFER_COUNT);
    }

    #[test]
    fn independent_cursor_owner_cannot_advance_or_corrupt_cursor() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("scratches");
        let first = next_buffer(&dir).unwrap();
        std::fs::write(&first, "π preserved").unwrap();
        let lease = CursorLease::acquire(&root.path().join("scratch-buffer-cursor.lock")).unwrap();
        let other = dir.clone();
        assert!(std::thread::spawn(move || next_buffer(&other))
            .join()
            .unwrap()
            .is_err());
        assert_eq!(
            std::fs::read_to_string(root.path().join("scratch-buffer-next-slot")).unwrap(),
            "1\n"
        );
        drop(lease);
        assert_eq!(
            next_buffer(&dir).unwrap().file_name().unwrap(),
            "buffer2.txt"
        );
        assert_eq!(std::fs::read_to_string(first).unwrap(), "π preserved");
    }

    #[test]
    fn buffer_slots_reuse_contents_and_preserve_changed_language() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("scratches");
        let first = next_buffer(&dir).unwrap();
        std::fs::write(&first, "π🙂 preserved").unwrap();
        let renamed = change_language(&first, "py").unwrap();
        for n in 2..=BUFFER_COUNT {
            assert_eq!(
                next_buffer(&dir).unwrap().file_name().unwrap(),
                format!("buffer{n}.txt").as_str()
            );
        }
        assert_eq!(next_buffer(&dir).unwrap(), renamed);
        assert_eq!(std::fs::read_to_string(renamed).unwrap(), "π🙂 preserved");
        assert_eq!(list(&dir).len(), BUFFER_COUNT);
        assert_eq!(
            std::fs::read_to_string(root.path().join("scratch-buffer-next-slot")).unwrap(),
            "1\n"
        );
    }
}

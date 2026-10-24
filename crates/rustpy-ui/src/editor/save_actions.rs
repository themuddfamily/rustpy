//! What happens to a document when it is saved (roadmap §0.2 "Trailing
//! whitespace stripping and final newline enforcement on save", §1.6
//! EditorConfig and "safe write"): strip trailing spaces (none / modified
//! lines / all), ensure a final line break, apply EditorConfig `end_of_line`
//! and `charset`, and write through a temporary file.

use std::collections::HashSet;
use std::path::Path;

use rustpy_core::{TextRange, TextSize};
use rustpy_settings::editorconfig::EditorConfigProps;
use rustpy_settings::StripTrailing;
use rustpy_text::{Document, Edit, Encoding, LineEnding};

/// Save actions for one file: the IDE settings, overridden by its EditorConfig.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SavePolicy {
    pub strip: StripTrailing,
    pub final_newline: bool,
    pub remove_final_newline: bool,
    /// EditorConfig `end_of_line`.
    pub line_ending: Option<LineEnding>,
    /// EditorConfig `charset`.
    pub encoding: Option<Encoding>,
}

impl SavePolicy {
    /// `trim_trailing_whitespace` (true = all lines, false = none) and
    /// `insert_final_newline` win over the settings.
    pub fn resolve(
        strip_setting: StripTrailing,
        ensure_newline: bool,
        ec: &EditorConfigProps,
    ) -> Self {
        let strip = match ec.trim_trailing_whitespace {
            Some(true) => StripTrailing::All,
            Some(false) => StripTrailing::None,
            None => strip_setting,
        };
        Self {
            strip,
            final_newline: ec.insert_final_newline.unwrap_or(ensure_newline),
            remove_final_newline: ec.insert_final_newline == Some(false),
            line_ending: ec.end_of_line.as_deref().and_then(parse_end_of_line),
            encoding: ec.charset.as_deref().and_then(parse_charset),
        }
    }
}

/// EditorConfig `end_of_line` value.
pub fn parse_end_of_line(value: &str) -> Option<LineEnding> {
    match value.trim().to_ascii_lowercase().as_str() {
        "lf" => Some(LineEnding::Lf),
        "crlf" => Some(LineEnding::CrLf),
        "cr" => Some(LineEnding::Cr),
        _ => None,
    }
}

/// EditorConfig `charset` value.
pub fn parse_charset(value: &str) -> Option<Encoding> {
    match value.trim().to_ascii_lowercase().as_str() {
        "utf-8" => Some(Encoding::Utf8),
        "utf-8-bom" => Some(Encoding::Utf8Bom),
        "latin1" => Some(Encoding::Latin1),
        "utf-16le" => Some(Encoding::Utf16Le),
        "utf-16be" => Some(Encoding::Utf16Be),
        _ => None,
    }
}

/// Whether `text` survives a round trip through `encoding`.
pub fn can_encode(text: &str, encoding: Encoding) -> bool {
    encoding != Encoding::Latin1 || text.chars().all(|c| (c as u32) < 256)
}

/// 0-based lines of `new` that are not unchanged lines of `old` (inserted or edited).
pub fn changed_lines(old: &str, new: &str) -> HashSet<usize> {
    let diff = similar::TextDiff::configure()
        .timeout(std::time::Duration::from_millis(500))
        .diff_lines(old, new);
    diff.iter_all_changes()
        .filter(|c| c.tag() == similar::ChangeTag::Insert)
        .filter_map(|c| c.new_index())
        .collect()
}

/// Ranges of Python string-literal tokens (their content may end lines with
/// significant whitespace).
pub fn python_string_ranges(text: &str) -> Vec<TextRange> {
    use rustpy_syntax::SyntaxKind;
    rustpy_syntax::tokenize(text)
        .0
        .into_iter()
        .filter(|t| {
            matches!(
                t.kind,
                SyntaxKind::STRING | SyntaxKind::FSTRING_MIDDLE | SyntaxKind::TSTRING_MIDDLE
            )
        })
        .map(|t| t.range)
        .collect()
}

/// Deletions of trailing spaces and tabs on `lines` (every line when `None`),
/// except on the `keep` lines (the caret lines) and where the whitespace lies
/// inside one of the sorted `strings` ranges (multi-line string literals).
pub fn trailing_ws_edits(
    doc: &Document,
    lines: Option<&HashSet<usize>>,
    keep: &HashSet<usize>,
    strings: &[TextRange],
) -> Vec<Edit> {
    let count = doc.line_count();
    let candidates: Vec<usize> = match lines {
        Some(set) => {
            let mut v: Vec<usize> = set.iter().copied().filter(|l| *l < count).collect();
            v.sort_unstable();
            v
        }
        None => (0..count).collect(),
    };
    let mut edits = Vec::new();
    for line in candidates {
        if keep.contains(&line) {
            continue;
        }
        let text = doc.line_text(line);
        let trimmed = text.trim_end_matches([' ', '\t']).len();
        if trimmed == text.len() {
            continue;
        }
        let start = doc.line_range(line).start();
        let range = TextRange::new(
            start + TextSize::new(trimmed as u32),
            start + TextSize::new(text.len() as u32),
        );
        let i = strings.partition_point(|s| s.start() < range.end());
        if i > 0 && strings[i - 1].end() > range.start() {
            continue;
        }
        edits.push(Edit::delete(range));
    }
    edits
}

/// The edits to make before writing `doc`: trailing whitespace per `policy`
/// and the final line break, in document order. `disk_text` is the file's
/// current content, which decides the "modified lines" (`None`: a new file,
/// every line counts as modified).
pub fn save_edits(
    doc: &Document,
    policy: &SavePolicy,
    disk_text: Option<&str>,
    keep: &HashSet<usize>,
    strings: &[TextRange],
) -> Vec<Edit> {
    let mut edits = match (policy.strip, disk_text) {
        (StripTrailing::None, _) => Vec::new(),
        (StripTrailing::ModifiedLines, Some(old)) => {
            trailing_ws_edits(doc, Some(&changed_lines(old, &doc.text())), keep, strings)
        }
        (StripTrailing::All | StripTrailing::ModifiedLines, _) => {
            trailing_ws_edits(doc, None, keep, strings)
        }
    };
    if policy.remove_final_newline {
        let mut start = doc.len();
        while let Some(ch) = doc.char_before(start) {
            let previous = start - TextSize::new(ch.len_utf8() as u32);
            if ch == '\n'
                || edits
                    .iter()
                    .any(|edit| edit.text.is_empty() && edit.range.contains(previous))
            {
                start = previous;
            } else {
                break;
            }
        }
        if start != doc.len() {
            edits.retain(|edit| edit.range.end() <= start);
            edits.push(Edit::delete(TextRange::new(start, doc.len())));
        }
    }
    if policy.final_newline && !doc.is_empty() && doc.char_before(doc.len()) != Some('\n') {
        edits.push(Edit::insert(doc.len(), "\n"));
    }
    edits
}

/// Atomic replacement for ordinary files, following symlinks and keeping permissions.
/// Hard links retain their shared inode and therefore require an in-place write.
/// No failed atomic write falls back to truncating the original file.
pub fn safe_write(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    write_impl(path, bytes, || Ok(()))
}

/// Rejects unacknowledged disk changes, preserving both disk and editor bytes.
/// The revision is checked again before replacement. Portable filesystems offer
/// no compare-and-swap rename: another process can still write after that check.
pub fn checked_write(
    vfs: &rustpy_core::vfs::Vfs,
    path: &Path,
    bytes: &[u8],
    expected: Option<rustpy_core::vfs::ContentRevision>,
    limits: rustpy_core::vfs::ReadLimits,
) -> std::io::Result<rustpy_core::vfs::ContentRevision> {
    let check = || {
        let observed = match vfs.read(path, limits) {
            Ok(snapshot) => Some(snapshot.revision),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(error),
        };
        if observed != expected {
            return Err(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                "file changed on disk; reload or explicitly Keep the editor version before saving",
            ));
        }
        Ok(())
    };
    check()?;
    write_impl(path, bytes, check)?;
    Ok(rustpy_core::vfs::ContentRevision::of(bytes))
}
fn write_impl(
    path: &Path,
    bytes: &[u8],
    check: impl Fn() -> std::io::Result<()>,
) -> std::io::Result<()> {
    use std::io::Write;
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let target = rustpy_core::vfs::normalize_path(path);
    let meta = match std::fs::metadata(&target) {
        Ok(m) => Some(m),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(e),
    };
    let mut _original: Option<std::fs::File> = None;
    if let Some(meta) = &meta {
        if !meta.is_file() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "not a regular file",
            ));
        }
        if meta.permissions().readonly() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "file is read-only",
            ));
        }
        // Only the Unix hard-link path writes through the original; on Windows an
        // open would race another process's atomic replacement (Access is denied).
        #[cfg(unix)]
        {
            _original = Some(rustpy_core::vfs::open_regular_file_for_write(&target)?);
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if meta.as_ref().is_some_and(|m| m.nlink() > 1) {
            check()?;
            let file = _original.as_mut().unwrap();
            file.set_len(0)?;
            file.write_all(bytes)?;
            return file.sync_all();
        }
    }
    let dir = target.parent().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "no parent directory")
    })?;
    let name = target
        .file_name()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "no file name"))?;
    let (tmp, mut file) = loop {
        let nonce = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let tmp = dir.join(format!(
            ".{}.rustpy-save-{}-{nonce}",
            name.to_string_lossy(),
            std::process::id()
        ));
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)
        {
            Ok(file) => break (tmp, file),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    };
    let result = (|| {
        file.write_all(bytes)?;
        if let Some(meta) = &meta {
            file.set_permissions(meta.permissions())?;
        }
        file.sync_all()?;
        check()?;
        // Windows refuses to replace a file another handle has open without
        // FILE_SHARE_DELETE (an indexer, a concurrent save): retry briefly.
        // ERROR_ACCESS_DENIED, ERROR_SHARING_VIOLATION (32) and ERROR_LOCK_VIOLATION
        // (33) are all transient here; FlushFileBuffers makes concurrent savers hold
        // the file for a while, so the budget is generous (about two seconds).
        let transient = |e: &std::io::Error| {
            e.kind() == std::io::ErrorKind::PermissionDenied
                || matches!(e.raw_os_error(), Some(32) | Some(33))
        };
        let mut attempt = 0;
        loop {
            match std::fs::rename(&tmp, &target) {
                Err(e) if cfg!(windows) && transient(&e) && attempt < 200 => {
                    attempt += 1;
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                other => break other,
            }
        }
    })();
    drop(file);
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_editorconfig_false_removes_final_newlines_after_whitespace_edits() {
        let mut doc = Document::new("value = 1   \n \n\n");
        let config = EditorConfigProps {
            insert_final_newline: Some(false),
            trim_trailing_whitespace: Some(true),
            ..Default::default()
        };
        let policy = SavePolicy::resolve(StripTrailing::None, true, &config);
        let edits = save_edits(&doc, &policy, None, &HashSet::new(), &[]);
        apply(&mut doc, edits);
        assert_eq!(doc.text(), "value = 1");
        assert!(doc.undo().is_some());
        assert_eq!(doc.text(), "value = 1   \n \n\n");
        let policy = SavePolicy::resolve(StripTrailing::None, false, &EditorConfigProps::default());
        assert!(
            save_edits(&doc, &policy, None, &HashSet::new(), &[]).is_empty(),
            "ordinary settings do not strip authored final lines"
        );
        let doc = Document::new("");
        let policy = SavePolicy::resolve(StripTrailing::None, true, &EditorConfigProps::default());
        assert!(save_edits(&doc, &policy, None, &HashSet::new(), &[]).is_empty());
    }

    fn apply(doc: &mut Document, edits: Vec<Edit>) {
        doc.edit(edits, &[], &[], rustpy_text::EditGroup::Separate, None)
            .unwrap();
    }

    fn policy(strip: StripTrailing, final_newline: bool) -> SavePolicy {
        SavePolicy {
            strip,
            final_newline,
            remove_final_newline: false,
            line_ending: None,
            encoding: None,
        }
    }

    #[test]
    fn strips_all_lines_but_the_caret_line() {
        let mut doc = Document::new("a  \nb\t\n  \nc   ");
        let keep: HashSet<usize> = [3].into();
        let edits = save_edits(&doc, &policy(StripTrailing::All, true), None, &keep, &[]);
        apply(&mut doc, edits);
        assert_eq!(doc.text(), "a\nb\n\nc   \n");
    }

    #[test]
    fn strips_only_modified_lines() {
        let disk = "keep  \nold\n";
        let mut doc = Document::new("keep  \nnew  \nadded \n");
        let edits = save_edits(
            &doc,
            &policy(StripTrailing::ModifiedLines, false),
            Some(disk),
            &HashSet::new(),
            &[],
        );
        apply(&mut doc, edits);
        assert_eq!(
            doc.text(),
            "keep  \nnew\nadded\n",
            "the untouched line keeps its spaces"
        );
        // A new file: every line counts as modified.
        let mut fresh = Document::new("x  \n");
        let edits = save_edits(
            &fresh,
            &policy(StripTrailing::ModifiedLines, false),
            None,
            &HashSet::new(),
            &[],
        );
        apply(&mut fresh, edits);
        assert_eq!(fresh.text(), "x\n");
        let none = save_edits(
            &Document::new("x  \n"),
            &policy(StripTrailing::None, true),
            None,
            &HashSet::new(),
            &[],
        );
        assert!(none.is_empty());
    }

    #[test]
    fn keeps_whitespace_inside_multiline_strings() {
        let text = "s = \"\"\"line  \nmore \"\"\"  \nx = 1  \n";
        let mut doc = Document::new(text);
        let strings = python_string_ranges(text);
        let edits = save_edits(
            &doc,
            &policy(StripTrailing::All, true),
            None,
            &HashSet::new(),
            &strings,
        );
        apply(&mut doc, edits);
        assert_eq!(doc.text(), "s = \"\"\"line  \nmore \"\"\"\nx = 1\n");
    }

    #[test]
    fn final_newline_with_stripped_last_line() {
        let mut doc = Document::new("a\nb  ");
        let edits = save_edits(
            &doc,
            &policy(StripTrailing::All, true),
            None,
            &HashSet::new(),
            &[],
        );
        apply(&mut doc, edits);
        assert_eq!(doc.text(), "a\nb\n");
        assert!(
            save_edits(
                &Document::new(""),
                &policy(StripTrailing::All, true),
                None,
                &HashSet::new(),
                &[]
            )
            .is_empty(),
            "empty files stay empty"
        );
    }

    #[test]
    fn editorconfig_wins_over_the_settings() {
        let ec = EditorConfigProps {
            trim_trailing_whitespace: Some(false),
            insert_final_newline: Some(false),
            end_of_line: Some("CRLF".into()),
            charset: Some("utf-8-bom".into()),
            ..Default::default()
        };
        let p = SavePolicy::resolve(StripTrailing::All, true, &ec);
        assert_eq!(
            p,
            SavePolicy {
                strip: StripTrailing::None,
                final_newline: false,
                remove_final_newline: true,
                line_ending: Some(LineEnding::CrLf),
                encoding: Some(Encoding::Utf8Bom)
            }
        );
        let ec = EditorConfigProps {
            trim_trailing_whitespace: Some(true),
            ..Default::default()
        };
        assert_eq!(
            SavePolicy::resolve(StripTrailing::ModifiedLines, true, &ec).strip,
            StripTrailing::All
        );
        let p = SavePolicy::resolve(
            StripTrailing::ModifiedLines,
            true,
            &EditorConfigProps::default(),
        );
        assert_eq!(
            p,
            SavePolicy {
                strip: StripTrailing::ModifiedLines,
                final_newline: true,
                remove_final_newline: false,
                line_ending: None,
                encoding: None
            }
        );
        assert_eq!(parse_charset("latin1"), Some(Encoding::Latin1));
        assert!(can_encode("café", Encoding::Latin1) && !can_encode("€", Encoding::Latin1));
    }

    struct TempDir(std::path::PathBuf);

    impl TempDir {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "rustpy-{name}-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn checked_save_conflicts_and_atomic_failures_leave_original_bytes() {
        let dir = TempDir::new("checked-write");
        let path = dir.path().join("m.py");
        std::fs::write(&path, b"initial").unwrap();
        let vfs = rustpy_core::vfs::Vfs::new();
        let limits = rustpy_core::vfs::ReadLimits::default();
        let first = vfs.read(&path, limits).unwrap();
        std::fs::write(&path, b"external").unwrap();
        assert!(checked_write(&vfs, &path, b"editor", Some(first.revision), limits).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"external");
        let result = write_impl(&path, b"replacement", || {
            Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "late failure",
            ))
        });
        assert!(result.is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"external");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
        let current = vfs.read(&path, limits).unwrap();
        assert!(checked_write(&vfs, &path, b"editor", Some(current.revision), limits).is_ok());
        assert_eq!(std::fs::read(&path).unwrap(), b"editor");
    }
    #[test]
    fn concurrent_safe_writes_use_unique_temps_and_never_partial_replacements() {
        let dir = TempDir::new("concurrent-write");
        let path = dir.path().join("m.py");
        std::fs::write(&path, b"initial").unwrap();
        let old_temp = dir
            .path()
            .join(format!(".m.py.rustpy-save-{}", std::process::id()));
        std::fs::write(&old_temp, b"sentinel").unwrap();
        let threads: Vec<_> = (1u8..=8)
            .map(|byte| {
                let path = path.clone();
                std::thread::spawn(move || safe_write(&path, &vec![byte; 65536]).unwrap())
            })
            .collect();
        for thread in threads {
            thread.join().unwrap();
        }
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(bytes.len(), 65536);
        assert!(bytes.iter().all(|b| *b == bytes[0]));
        assert_eq!(std::fs::read(old_temp).unwrap(), b"sentinel");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 2);
    }

    #[test]
    fn safe_write_replaces_content_and_keeps_permissions() {
        let dir = TempDir::new("safe-write");
        let path = dir.path().join("f.py");
        std::fs::write(&path, "old").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o750)).unwrap();
        }
        safe_write(&path, b"new").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "new");
        assert_eq!(
            std::fs::read_dir(dir.path()).unwrap().count(),
            1,
            "no temporary file left behind"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o750
            );
            // A symlink stays a symlink; its target gets the content.
            let link = dir.path().join("link.py");
            std::os::unix::fs::symlink(&path, &link).unwrap();
            safe_write(&link, b"via link").unwrap();
            assert!(std::fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink());
            assert_eq!(std::fs::read_to_string(&path).unwrap(), "via link");
            // A dangling symlink keeps its identity and creates its target.
            let missing = dir.path().join("missing.py");
            let dangling = dir.path().join("dangling.py");
            std::os::unix::fs::symlink(&missing, &dangling).unwrap();
            safe_write(&dangling, b"created").unwrap();
            assert!(std::fs::symlink_metadata(&dangling)
                .unwrap()
                .file_type()
                .is_symlink());
            assert_eq!(std::fs::read(&missing).unwrap(), b"created");
            // Hard links keep sharing the content.
            let hard = dir.path().join("hard.py");
            std::fs::hard_link(&path, &hard).unwrap();
            safe_write(&path, b"shared").unwrap();
            assert_eq!(std::fs::read_to_string(&hard).unwrap(), "shared");
        }
        // A new file is created.
        let fresh = dir.path().join("new.txt");
        safe_write(&fresh, b"x").unwrap();
        assert_eq!(std::fs::read_to_string(&fresh).unwrap(), "x");
    }
}

//! Plugin-relative paths: what a manifest or bundle may name, and resolving
//! those names to files that really are inside the plugin directory.

use std::fs;
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// Characters Windows does not allow in file names (paths must be portable).
const NOT_PORTABLE: &[char] = &['<', '>', ':', '"', '|', '?', '*'];

/// Deepest directory nesting a plugin may use: a path has at most
/// `MAX_DEPTH + 1` segments (16 directories and a file name).
pub(crate) const MAX_DEPTH: usize = 16;

/// Names Windows reserves for devices, with or without an extension
/// (`nul.json` opens the NUL device there); compared case-insensitively.
#[rustfmt::skip]
const WINDOWS_DEVICE_NAMES: &[&str] = &[
    "con", "prn", "aux", "nul", "conin$", "conout$", "com0", "com1", "com2", "com3", "com4",
    "com5", "com6", "com7", "com8", "com9", "com\u{b9}", "com\u{b2}", "com\u{b3}", "lpt0", "lpt1",
    "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9", "lpt\u{b9}", "lpt\u{b2}",
    "lpt\u{b3}",
];

/// True when Windows treats the file name `name` as a device: one of the
/// reserved names, alone or followed by an extension (`CON`, `nul.txt`,
/// `com1.tar.gz`, `lpt1 .x`).
pub(crate) fn is_windows_device_name(name: &str) -> bool {
    let stem = name.split('.').next().unwrap_or(name).trim_end_matches(' ');
    let stem = stem.to_lowercase();
    WINDOWS_DEVICE_NAMES.contains(&stem.as_str())
}

/// Checks a path written in a manifest or bundle (`colors/dark.json`): it must
/// be relative, use `/` as separator, never leave the plugin directory (`..`),
/// nest at most [`MAX_DEPTH`] directories and only use file names that work on
/// every OS (no Windows device names such as `con` or `nul.json`). Returns the
/// path to join onto the plugin directory; `.` and empty segments are dropped.
pub(crate) fn safe_relative_path(raw: &str) -> Result<PathBuf, String> {
    if raw.trim().is_empty() {
        return Err("the path is empty".into());
    }
    if raw.chars().any(char::is_control) {
        return Err(format!(
            "`{}` contains control characters",
            raw.escape_debug()
        ));
    }
    if raw.contains('\\') {
        return Err(format!("`{raw}` uses `\\`; separate directories with `/`"));
    }
    let bytes = raw.as_bytes();
    if raw.starts_with('/')
        || (bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':')
    {
        return Err(format!(
            "`{raw}` is an absolute path; paths must be relative to the plugin directory"
        ));
    }
    let mut out = PathBuf::new();
    for segment in raw.split('/') {
        match segment {
            "" | "." => continue,
            ".." => {
                return Err(format!(
                    "`{raw}` points outside the plugin directory (`..` is not allowed)"
                ))
            }
            _ => {}
        }
        if segment.chars().all(|c| c == '.') {
            return Err(format!(
                "`{raw}` contains the invalid file name `{segment}`"
            ));
        }
        if let Some(c) = segment.chars().find(|c| NOT_PORTABLE.contains(c)) {
            return Err(format!(
                "`{raw}` contains `{c}`, which is not allowed in file names"
            ));
        }
        if segment.ends_with(['.', ' ']) {
            return Err(format!(
                "`{raw}`: file names must not end with a dot or a space (`{segment}`)"
            ));
        }
        if is_windows_device_name(segment) {
            return Err(format!(
                "`{raw}` uses `{segment}`, a name Windows reserves for a device"
            ));
        }
        out.push(segment);
        if out.components().count() > MAX_DEPTH + 1 {
            return Err(format!(
                "`{raw}` is nested more than {MAX_DEPTH} directories deep"
            ));
        }
    }
    if out.as_os_str().is_empty() {
        return Err(format!("`{raw}` does not name a file"));
    }
    debug_assert!(out.components().all(|c| matches!(c, Component::Normal(_))));
    Ok(out)
}

/// Resolves `raw` (already accepted by [`safe_relative_path`]) inside `dir` and
/// checks it is an existing regular file that — after following symbolic
/// links — still lies inside `dir`. Returns `dir.join(raw)`.
pub(crate) fn existing_file_inside(dir: &Path, raw: &str) -> Result<PathBuf, String> {
    let rel = safe_relative_path(raw)?;
    let path = dir.join(rel);
    let meta = match fs::metadata(&path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(format!("`{raw}` does not exist in the plugin directory"))
        }
        Err(e) => return Err(format!("`{raw}`: {e}")),
    };
    if !meta.is_file() {
        return Err(format!("`{raw}` is not a regular file"));
    }
    let real_dir = dunce::canonicalize(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let real = dunce::canonicalize(&path).map_err(|e| format!("`{raw}`: {e}"))?;
    if !real.starts_with(&real_dir) {
        return Err(format!(
            "`{raw}` is a link to a file outside the plugin directory"
        ));
    }
    Ok(path)
}

/// Reads a UTF-8 text file of at most `max` bytes.
pub(crate) fn read_text_limited(path: &Path, max: u64, shown: &str) -> Result<String, String> {
    let file = fs::File::open(path).map_err(|e| format!("`{shown}`: {e}"))?;
    let mut bytes = Vec::new();
    file.take(max + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("`{shown}`: {e}"))?;
    if bytes.len() as u64 > max {
        return Err(format!("`{shown}` is larger than {} KiB", max / 1024));
    }
    String::from_utf8(bytes).map_err(|_| format!("`{shown}` is not UTF-8 text"))
}

/// `path` made absolute against the current directory (without touching the
/// file system, so it also works for paths that do not exist yet).
pub(crate) fn absolute(path: &Path) -> PathBuf {
    std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf())
}

/// A suffix for temporary names that is unique within the machine.
pub(crate) fn unique_suffix() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!(
        "{}-{nanos}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_plain_relative_paths() {
        assert_eq!(
            safe_relative_path("colors/dark.json").unwrap(),
            PathBuf::from("colors").join("dark.json")
        );
        assert_eq!(
            safe_relative_path("./a//b.json").unwrap(),
            PathBuf::from("a").join("b.json")
        );
        assert_eq!(
            safe_relative_path("plugin.json").unwrap(),
            PathBuf::from("plugin.json")
        );
        assert!(safe_relative_path(".hidden/file").is_ok());
    }

    #[test]
    fn rejects_escapes_and_non_portable_names() {
        for bad in [
            "",
            "  ",
            "/etc/passwd",
            "../x.json",
            "a/../../x",
            "a/..",
            "C:/x.json",
            "c:x",
            "colors\\x.json",
            "\\\\server\\share",
            "a/.../b",
            "a\nb",
            "con:x",
            "a/b?.json",
            "dir./x",
            "x ",
            ".",
            "./",
        ] {
            assert!(
                safe_relative_path(bad).is_err(),
                "{bad:?} should be rejected"
            );
        }
        assert!(safe_relative_path("../x")
            .unwrap_err()
            .contains("outside the plugin directory"));
        assert!(safe_relative_path("/x").unwrap_err().contains("absolute"));
    }

    #[test]
    fn windows_device_names_are_rejected_in_every_segment() {
        for bad in [
            "con",
            "CON",
            "nul.json",
            "aux/x.json",
            "a/com1.txt",
            "lpt1",
            "Lpt9.tar.gz",
            "com0",
            "com\u{b9}.txt",
            "prn/x",
            "conin$",
            "CONOUT$.log",
            "lpt1 .txt",
        ] {
            let err = safe_relative_path(bad).expect_err(bad);
            assert!(err.contains("reserves for a device"), "{bad:?}: {err}");
        }
        for ok in [
            "console.json",
            "conx",
            "com10.txt",
            "nulled/x.json",
            "lpt",
            "a.con",
            "x/aux-tools.json",
        ] {
            assert!(safe_relative_path(ok).is_ok(), "{ok:?} should be accepted");
        }
    }

    #[test]
    fn paths_nest_at_most_max_depth_directories() {
        let deepest = format!("{}/x.json", vec!["d"; MAX_DEPTH].join("/"));
        assert!(safe_relative_path(&deepest).is_ok());
        let too_deep = format!("d/{deepest}");
        assert!(safe_relative_path(&too_deep)
            .unwrap_err()
            .contains("nested more than 16 directories deep"));
        // `.` and empty segments do not count.
        let dotted = format!("./{}/x.json", vec!["d"; MAX_DEPTH].join("//"));
        assert!(safe_relative_path(&dotted).is_ok());
    }

    #[test]
    fn unique_suffixes_differ() {
        assert_ne!(unique_suffix(), unique_suffix());
    }
}

//! `file:` URIs ⇄ paths, and the on-disk copies of the bundled typeshed stubs.
//!
//! The analysis knows stdlib stubs by virtual paths (`<typeshed>/os/path.pyi`).
//! Editors can only open real files, so navigation into a stub writes it to a
//! cache directory first, and requests on such a file map back to the
//! virtual path (so imports inside the stub keep resolving).

use std::path::{Path, PathBuf};
use std::str::FromStr;

use lsp_types::Uri;

const TYPESHED: &str = "<typeshed>";

pub fn path_from_uri(uri: &Uri) -> Option<PathBuf> {
    let url = url::Url::parse(uri.as_str()).ok()?;
    if url.scheme() != "file" {
        return None;
    }
    url.to_file_path().ok()
}

pub fn uri_from_path(path: &Path) -> Option<Uri> {
    let url = url::Url::from_file_path(path).ok()?;
    Uri::from_str(url.as_str()).ok()
}

pub fn is_virtual_stub(path: &Path) -> bool {
    path.starts_with(TYPESHED)
}

/// Where stubs are written: `<dir>/os/path.pyi` for `<typeshed>/os/path.pyi`.
#[derive(Clone, Debug)]
pub struct StubDir {
    root: PathBuf,
}

impl StubDir {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    /// Whether a filesystem event belongs to this generated typeshed cache.
    /// Normalize existing ancestors and missing suffixes so aliases and deleted
    /// files match, while parent escapes and similarly named siblings do not.
    pub fn contains_generated_path(&self, path: &Path) -> bool {
        rustpy_core::vfs::normalize_path(path)
            .starts_with(rustpy_core::vfs::normalize_path(&self.root))
    }

    /// The default location: `rustpy/typeshed` in the user's cache directory
    /// (inside `RUSTPY_CONFIG_DIR` when that is set).
    pub fn default_location() -> PathBuf {
        if std::env::var_os("RUSTPY_CONFIG_DIR").is_some() {
            return rustpy_settings::Settings::config_dir()
                .join("cache")
                .join("typeshed");
        }
        match user_cache_dir() {
            Some(dir) => dir.join("rustpy").join("typeshed"),
            None => std::env::temp_dir().join("rustpy-typeshed"),
        }
    }

    /// The real file showing virtual stub `virtual_path` (with `text`),
    /// written (or refreshed) on demand.
    pub fn materialize(&self, virtual_path: &Path, text: &str) -> Option<PathBuf> {
        let rel = virtual_path.strip_prefix(TYPESHED).ok()?;
        if rel
            .components()
            .any(|c| !matches!(c, std::path::Component::Normal(_)))
        {
            return None;
        }
        let real = self.root.join(rel);
        if std::fs::read_to_string(&real).ok().as_deref() != Some(text) {
            if let Some(parent) = real.parent() {
                std::fs::create_dir_all(parent).ok()?;
            }
            // Write a temporary file and rename it so readers never see half a stub.
            let tmp = real.with_extension(format!("pyi.{}.tmp", std::process::id()));
            std::fs::write(&tmp, text).ok()?;
            if std::fs::rename(&tmp, &real).is_err() {
                let _ = std::fs::remove_file(&tmp);
                return None;
            }
        }
        Some(real)
    }

    /// The virtual stub path of a file in this directory (`None` for other files).
    pub fn virtual_path(&self, real: &Path) -> Option<PathBuf> {
        let rel = real.strip_prefix(&self.root).ok()?;
        (rel.extension().and_then(|e| e.to_str()) == Some("pyi")).then(|| {
            let rel = rel.to_string_lossy().replace('\\', "/");
            PathBuf::from(format!("{TYPESHED}/{rel}"))
        })
    }
}

/// The dotted module name of a virtual stub path (`<typeshed>/os/path.pyi` → `os.path`).
pub fn stub_module_name(virtual_path: &Path) -> Option<String> {
    let rel = virtual_path.strip_prefix(TYPESHED).ok()?;
    let rel = rel.to_string_lossy().replace('\\', "/");
    let rel = rel.strip_suffix(".pyi")?;
    let rel = rel.strip_suffix("/__init__").unwrap_or(rel);
    Some(rel.replace('/', "."))
}

/// `~/Library/Caches` on macOS, `%LOCALAPPDATA%` on Windows, `$XDG_CACHE_HOME`
/// or `~/.cache` elsewhere.
fn user_cache_dir() -> Option<PathBuf> {
    if cfg!(target_os = "macos") {
        std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Library").join("Caches"))
    } else if cfg!(windows) {
        std::env::var_os("LOCALAPPDATA").map(PathBuf::from)
    } else {
        std::env::var_os("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uris_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("my project").join("ünï.py");
        let uri = uri_from_path(&path).unwrap();
        assert!(uri.as_str().starts_with("file://"));
        assert!(uri.as_str().contains("my%20project"));
        assert_eq!(path_from_uri(&uri).unwrap(), path);
        assert!(path_from_uri(&Uri::from_str("untitled:Untitled-1").unwrap()).is_none());
    }

    #[test]
    fn stubs_materialize_and_map_back() {
        let dir = tempfile::tempdir().unwrap();
        let stubs = StubDir::new(dir.path().join("typeshed"));
        let virt = Path::new("<typeshed>/os/path.pyi");
        let real = stubs
            .materialize(virt, "def join(a: str) -> str: ...\n")
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(&real).unwrap(),
            "def join(a: str) -> str: ...\n"
        );
        assert_eq!(stubs.virtual_path(&real).unwrap(), virt);
        assert_eq!(stub_module_name(virt).as_deref(), Some("os.path"));
        assert_eq!(
            stub_module_name(Path::new("<typeshed>/os.pyi")).as_deref(),
            Some("os")
        );
        assert!(stubs
            .materialize(Path::new("<typeshed>/../evil.pyi"), "")
            .is_none());
        assert!(stubs.virtual_path(Path::new("/elsewhere/x.pyi")).is_none());
    }

    #[test]
    fn generated_cache_ownership_uses_normalized_component_boundaries() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let cache = root.join(".stubs");
        std::fs::create_dir(&cache).unwrap();
        let stubs = StubDir::new(cache.clone());
        assert!(stubs.contains_generated_path(&cache));
        assert!(stubs.contains_generated_path(&cache.join("missing/pkg.pyi")));
        assert!(stubs.contains_generated_path(&root.join("pkg/../.stubs/deleted.pyi")));
        assert!(!stubs.contains_generated_path(&root));
        assert!(!stubs.contains_generated_path(&root.join(".stubs-other/user.pyi")));
        assert!(!stubs.contains_generated_path(&cache.join("../user.pyi")));
        assert!(!stubs.contains_generated_path(&root.join("user/.stubs/user.pyi")));
    }

    #[cfg(unix)]
    #[test]
    fn generated_cache_ownership_resolves_aliases_but_not_links_to_user_stubs() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let cache = root.join("typeshed");
        std::fs::create_dir(&cache).unwrap();
        let alias = root.join("cache-alias");
        symlink(&cache, &alias).unwrap();
        let stubs = StubDir::new(alias.clone());
        assert!(stubs.contains_generated_path(&cache.join("deleted.pyi")));
        assert!(stubs.contains_generated_path(&alias.join("deleted.pyi")));
        let user = root.join("user.pyi");
        std::fs::write(&user, "class User: ...\n").unwrap();
        symlink(&user, cache.join("linked.pyi")).unwrap();
        assert!(!stubs.contains_generated_path(&cache.join("linked.pyi")));
        assert!(!stubs.contains_generated_path(&alias.join("../user.pyi")));
    }
}

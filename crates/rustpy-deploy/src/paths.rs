//! Remote path arithmetic and local ↔ remote path mapping.
//!
//! Remote paths are POSIX strings: absolute (`/var/www/app`) or relative to the login
//! directory (`www/app`; `.` is the login directory itself, a leading `~/` means the same).
//! For a [`ServerKind::Local`](crate::ServerKind::Local) server they are relative to its folder.

use std::path::{Component, Path, PathBuf};

use crate::config::{Mapping, Server};

/// Normalizes a remote path: collapses `//`, drops `.` segments, resolves `..` lexically
/// (never above `/`), strips the trailing `/` and a leading `~` segment. Relative paths
/// that end up empty become `.`.
pub fn normalize_remote(path: &str) -> String {
    let absolute = path.starts_with('/');
    let mut parts: Vec<&str> = Vec::new();
    for seg in path.split('/') {
        match seg {
            "" | "." => {}
            "~" if !absolute && parts.is_empty() => {}
            ".." => {
                if parts.last().is_some_and(|p| *p != "..") {
                    parts.pop();
                } else if !absolute {
                    parts.push("..");
                }
            }
            s => parts.push(s),
        }
    }
    if absolute {
        format!("/{}", parts.join("/"))
    } else if parts.is_empty() {
        ".".to_string()
    } else {
        parts.join("/")
    }
}

/// Joins a relative path (a leading `/` is ignored) onto `base`, normalizing the result.
pub fn join_remote(base: &str, rel: &str) -> String {
    let rel = rel.trim_start_matches('/');
    let base = normalize_remote(base);
    if rel.is_empty() {
        base
    } else if base == "/" {
        normalize_remote(&format!("/{rel}"))
    } else {
        normalize_remote(&format!("{base}/{rel}"))
    }
}

/// Parent directory of a remote path; `None` for `/` and `.`.
pub fn remote_parent(path: &str) -> Option<String> {
    let p = normalize_remote(path);
    if p == "/" || p == "." || p == ".." || p.ends_with("/..") {
        return None;
    }
    Some(match p.rfind('/') {
        Some(0) => "/".to_string(),
        Some(i) => p[..i].to_string(),
        None => ".".to_string(),
    })
}

/// Last segment of a remote path (`""` for `/`).
pub fn remote_file_name(path: &str) -> String {
    let p = normalize_remote(path);
    if p == "/" || p == "." {
        return String::new();
    }
    p.rsplit('/').next().unwrap_or("").to_string()
}

/// True for paths that must never be deleted: `/`, the login directory, `..`.
pub(crate) fn is_root_like(path: &str) -> bool {
    let p = normalize_remote(path);
    p == "/" || p == "." || p == ".." || p.starts_with("../") || p.ends_with("/..")
}

/// `rest` when `path` lies below (or equals) `root`; both must be normalized.
pub(crate) fn remote_strip_prefix<'a>(path: &'a str, root: &str) -> Option<&'a str> {
    if root == "/" {
        return path.strip_prefix('/');
    }
    if root == "." {
        if path.starts_with('/') || path == ".." || path.starts_with("../") {
            return None;
        }
        return Some(if path == "." { "" } else { path });
    }
    if path == root {
        return Some("");
    }
    path.strip_prefix(root)
        .and_then(|rest| rest.strip_prefix('/'))
}

fn remote_depth(root: &str) -> usize {
    root.split('/')
        .filter(|s| !s.is_empty() && *s != ".")
        .count()
}

/// Lexically normalizes a local path (`.`/`..` segments) without touching the file system.
pub(crate) fn normalize_local(p: &Path) -> PathBuf {
    let mut head = PathBuf::new();
    let mut parts: Vec<std::ffi::OsString> = Vec::new();
    for c in p.components() {
        match c {
            Component::Prefix(_) | Component::RootDir => head.push(c.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                if parts.last().is_some_and(|l| l != "..") {
                    parts.pop();
                } else if head.as_os_str().is_empty() {
                    parts.push("..".into());
                }
            }
            Component::Normal(s) => parts.push(s.to_os_string()),
        }
    }
    for part in parts {
        head.push(part);
    }
    head
}

/// `path` relative to `root` as a `/`-separated string (`""` for `root` itself), or `None`
/// when `path` is not below `root`. Both should be normalized.
pub(crate) fn rel_string(root: &Path, path: &Path) -> Option<String> {
    let rest = path.strip_prefix(root).ok()?;
    Some(
        rest.components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("/"),
    )
}

/// Appends a `/`-separated relative path to a local path.
pub(crate) fn push_rel(base: &Path, rel: &str) -> PathBuf {
    let mut out = base.to_path_buf();
    for seg in rel.split('/').filter(|s| !s.is_empty() && *s != ".") {
        out.push(seg);
    }
    out
}

/// Absolute local directory of a mapping.
pub fn mapping_local_root(project_root: &Path, mapping: &Mapping) -> PathBuf {
    let local = mapping.local.trim();
    let as_path = Path::new(local);
    if as_path.is_absolute() {
        return normalize_local(as_path);
    }
    let mut abs = project_root.to_path_buf();
    for seg in local
        .split(['/', '\\'])
        .filter(|s| !s.is_empty() && *s != ".")
    {
        abs.push(seg);
    }
    normalize_local(&abs)
}

/// Remote directory of a mapping: `server.root_path` joined with `mapping.remote`.
pub fn mapping_remote_root(server: &Server, mapping: &Mapping) -> String {
    join_remote(server.root_path.trim(), mapping.remote.trim())
}

/// Remote path of `local_file` (absolute, or relative to `project_root`) on `server`.
///
/// The mapping with the longest local prefix wins; a server without mappings maps the whole
/// project to its `root_path`. `None` when no mapping covers the file.
pub fn remote_path(server: &Server, project_root: &Path, local_file: &Path) -> Option<String> {
    let file = normalize_local(&project_root.join(local_file));
    let mut best: Option<(usize, PathBuf, String)> = None;
    for m in server.effective_mappings() {
        let local_root = mapping_local_root(project_root, &m);
        if !file.starts_with(&local_root) {
            continue;
        }
        let depth = local_root.components().count();
        if best.as_ref().is_none_or(|(d, ..)| depth > *d) {
            best = Some((depth, local_root, mapping_remote_root(server, &m)));
        }
    }
    let (_, local_root, remote_root) = best?;
    let rel = rel_string(&local_root, &file)?;
    Some(join_remote(&remote_root, &rel))
}

/// Local path of a remote path on `server` (the inverse of [`remote_path`]): the mapping with
/// the longest remote prefix wins. `None` when no mapping covers `remote`.
pub fn local_path(server: &Server, project_root: &Path, remote: &str) -> Option<PathBuf> {
    let remote = normalize_remote(remote);
    let mut best: Option<(usize, PathBuf, String)> = None;
    for m in server.effective_mappings() {
        let root = mapping_remote_root(server, &m);
        let Some(rest) = remote_strip_prefix(&remote, &root) else {
            continue;
        };
        if rest.split('/').any(|s| s == "..") {
            continue;
        }
        let depth = remote_depth(&root);
        if best.as_ref().is_none_or(|(d, ..)| depth > *d) {
            best = Some((
                depth,
                mapping_local_root(project_root, &m),
                rest.to_string(),
            ));
        }
    }
    let (_, local_root, rest) = best?;
    Some(push_rel(&local_root, &rest))
}

/// Browser URL of `local_file`: `server.web_url` + its remote path relative to `root_path`.
pub fn web_url(server: &Server, project_root: &Path, local_file: &Path) -> Option<String> {
    let base = server
        .web_url
        .as_deref()
        .map(str::trim)
        .filter(|u| !u.is_empty())?;
    let file = normalize_local(&project_root.join(local_file));
    if let Some(mapping) = server
        .effective_mappings()
        .into_iter()
        .filter(|mapping| {
            !mapping.web_path.is_empty()
                && file.starts_with(mapping_local_root(project_root, mapping))
        })
        .max_by_key(|mapping| {
            mapping_local_root(project_root, mapping)
                .components()
                .count()
        })
    {
        let local = mapping_local_root(project_root, &mapping);
        let relative = file.strip_prefix(local).ok()?;
        let suffix = relative.to_string_lossy().replace('\\', "/");
        let path = format!("{}/{}", mapping.web_path.trim_matches('/'), suffix)
            .trim_matches('/')
            .split('/')
            .map(percent_encode_segment)
            .collect::<Vec<_>>()
            .join("/");
        return Some(format!("{}/{}", base.trim_end_matches('/'), path));
    }
    let remote = remote_path(server, project_root, local_file)?;
    let root = normalize_remote(server.root_path.trim());
    let rel = remote_strip_prefix(&remote, &root)?;
    if rel.is_empty() {
        return Some(base.to_string());
    }
    let encoded: Vec<String> = rel.split('/').map(percent_encode_segment).collect();
    Some(format!(
        "{}/{}",
        base.trim_end_matches('/'),
        encoded.join("/")
    ))
}

/// Percent-encodes one URL path segment (RFC 3986 unreserved characters and a few safe
/// sub-delimiters are kept; `/`, `;`, `%`, `#`, `?`, `[`, `]`, spaces, quotes and non-ASCII
/// bytes are encoded).
pub fn percent_encode_segment(segment: &str) -> String {
    let mut out = String::with_capacity(segment.len());
    for b in segment.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~!$&()*+,=:@".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Server, ServerKind};

    #[test]
    fn normalize() {
        assert_eq!(normalize_remote("/var//www/./app/"), "/var/www/app");
        assert_eq!(normalize_remote("/"), "/");
        assert_eq!(normalize_remote(""), ".");
        assert_eq!(normalize_remote("."), ".");
        assert_eq!(normalize_remote("~"), ".");
        assert_eq!(normalize_remote("~/www"), "www");
        assert_eq!(normalize_remote("./~/www"), "www");
        assert_eq!(normalize_remote("a/~/b"), "a/~/b");
        assert_eq!(normalize_remote("~user/www"), "~user/www");
        assert_eq!(normalize_remote("/a/../../b"), "/b");
        assert_eq!(normalize_remote("a/../../b"), "../b");
        assert_eq!(normalize_remote("a/b/.."), "a");
        assert_eq!(normalize_remote("/my app/it's"), "/my app/it's");
    }

    #[test]
    fn join_parent_name() {
        assert_eq!(join_remote("/var/www", "app/x.py"), "/var/www/app/x.py");
        assert_eq!(join_remote("/", "x.py"), "/x.py");
        assert_eq!(join_remote(".", "x.py"), "x.py");
        assert_eq!(join_remote("~/site", "/x.py"), "site/x.py");
        assert_eq!(join_remote("/var/www", ""), "/var/www");
        assert_eq!(join_remote("", ""), ".");
        assert_eq!(remote_parent("/var/www/x.py").as_deref(), Some("/var/www"));
        assert_eq!(remote_parent("/x.py").as_deref(), Some("/"));
        assert_eq!(remote_parent("x.py").as_deref(), Some("."));
        assert_eq!(remote_parent("a/b").as_deref(), Some("a"));
        assert_eq!(remote_parent("/"), None);
        assert_eq!(remote_parent("."), None);
        assert_eq!(remote_file_name("/var/www/x y.py"), "x y.py");
        assert_eq!(remote_file_name("/"), "");
        assert!(is_root_like("/a/.."));
        assert!(is_root_like("~"));
        assert!(!is_root_like("/a"));
    }

    #[test]
    fn local_normalization() {
        assert_eq!(
            normalize_local(Path::new("/a/./b/../c")),
            PathBuf::from("/a/c")
        );
        assert_eq!(normalize_local(Path::new("/../a")), PathBuf::from("/a"));
        assert_eq!(
            normalize_local(Path::new("a/../../b")),
            PathBuf::from("../b")
        );
        assert_eq!(
            rel_string(Path::new("/p"), Path::new("/p/a/b.py")).as_deref(),
            Some("a/b.py")
        );
        assert_eq!(
            rel_string(Path::new("/p"), Path::new("/p")).as_deref(),
            Some("")
        );
        assert_eq!(rel_string(Path::new("/p"), Path::new("/px/a")), None);
    }

    fn server() -> Server {
        Server::sftp("prod", "example.com", "deploy", "/var/www/app")
    }

    #[test]
    fn default_mapping() {
        let root = Path::new("/home/me/proj");
        let s = server();
        assert_eq!(
            remote_path(&s, root, Path::new("/home/me/proj/pkg/mod.py")).as_deref(),
            Some("/var/www/app/pkg/mod.py")
        );
        assert_eq!(
            remote_path(&s, root, Path::new("pkg/mod.py")).as_deref(),
            Some("/var/www/app/pkg/mod.py"),
            "relative to the project"
        );
        assert_eq!(
            remote_path(&s, root, Path::new("/home/me/proj/./a/../b c.py")).as_deref(),
            Some("/var/www/app/b c.py")
        );
        assert_eq!(remote_path(&s, root, root).as_deref(), Some("/var/www/app"));
        assert_eq!(
            remote_path(&s, root, Path::new("/home/me/other/x.py")),
            None
        );
        assert_eq!(
            remote_path(&s, root, Path::new("/home/me/projx/x.py")),
            None,
            "component-wise prefix"
        );
        assert_eq!(
            local_path(&s, root, "/var/www/app/pkg/mod.py"),
            Some(PathBuf::from("/home/me/proj/pkg/mod.py"))
        );
        assert_eq!(
            local_path(&s, root, "/var/www/app"),
            Some(PathBuf::from("/home/me/proj"))
        );
        assert_eq!(local_path(&s, root, "/var/www/application/x"), None);
        assert_eq!(local_path(&s, root, "/etc/passwd"), None);
    }

    #[test]
    fn longest_prefix_wins() {
        let root = Path::new("/home/me/proj");
        let s = server()
            .with_mapping("", "")
            .with_mapping("src", "code")
            .with_mapping("src/static", "/public/assets")
            .with_mapping("docs", "/");
        assert_eq!(
            remote_path(&s, root, Path::new("README.md")).as_deref(),
            Some("/var/www/app/README.md")
        );
        assert_eq!(
            remote_path(&s, root, Path::new("src/app.py")).as_deref(),
            Some("/var/www/app/code/app.py")
        );
        assert_eq!(
            remote_path(&s, root, Path::new("src/static/css/site.css")).as_deref(),
            Some("/var/www/app/public/assets/css/site.css"),
            "remote is relative to root_path"
        );
        assert_eq!(
            remote_path(&s, root, Path::new("src/staticx.py")).as_deref(),
            Some("/var/www/app/code/staticx.py")
        );
        assert_eq!(
            remote_path(&s, root, Path::new("docs/index.md")).as_deref(),
            Some("/var/www/app/index.md")
        );
        // Inverse: the longest remote prefix wins.
        assert_eq!(
            local_path(&s, root, "/var/www/app/code/app.py"),
            Some(PathBuf::from("/home/me/proj/src/app.py"))
        );
        assert_eq!(
            local_path(&s, root, "/var/www/app/public/assets/css/site.css"),
            Some(PathBuf::from("/home/me/proj/src/static/css/site.css"))
        );
        // "docs" -> "/" and "" -> "" both map to the root path; the first listed mapping wins ties.
        assert_eq!(
            local_path(&s, root, "/var/www/app/index.md"),
            Some(PathBuf::from("/home/me/proj/index.md"))
        );
    }

    #[test]
    fn explicit_mappings_replace_the_default() {
        let root = Path::new("/home/me/proj");
        let s = server().with_mapping("src", "");
        assert_eq!(
            remote_path(&s, root, Path::new("src/app.py")).as_deref(),
            Some("/var/www/app/app.py")
        );
        assert_eq!(remote_path(&s, root, Path::new("tests/test_app.py")), None);
        assert_eq!(
            local_path(&s, root, "/var/www/app/app.py"),
            Some(PathBuf::from("/home/me/proj/src/app.py"))
        );
    }

    #[test]
    fn relative_and_special_roots() {
        let root = Path::new("/p");
        let home = Server::rsync("r", "h", "u", "~/site");
        assert_eq!(
            remote_path(&home, root, Path::new("/p/a.py")).as_deref(),
            Some("site/a.py")
        );
        assert_eq!(
            local_path(&home, root, "site/a.py"),
            Some(PathBuf::from("/p/a.py"))
        );
        assert_eq!(
            local_path(&home, root, "~/site/a.py"),
            Some(PathBuf::from("/p/a.py"))
        );
        assert_eq!(local_path(&home, root, "/site/a.py"), None);

        let login = Server::rsync("r", "h", "u", "");
        assert_eq!(
            remote_path(&login, root, Path::new("/p/a.py")).as_deref(),
            Some("a.py")
        );
        assert_eq!(
            local_path(&login, root, "a.py"),
            Some(PathBuf::from("/p/a.py"))
        );
        assert_eq!(local_path(&login, root, "../a.py"), None);

        let slash = Server::local("m", "/mnt/www");
        assert_eq!(
            remote_path(&slash, root, Path::new("/p/pkg/a.py")).as_deref(),
            Some("/pkg/a.py")
        );
        assert_eq!(
            local_path(&slash, root, "/pkg/a.py"),
            Some(PathBuf::from("/p/pkg/a.py"))
        );

        let abs_local = Server::new(
            "x",
            ServerKind::Local {
                root: "/mnt".into(),
            },
            "/",
        )
        .with_mapping("/shared/lib", "lib");
        assert_eq!(
            remote_path(&abs_local, root, Path::new("/shared/lib/u.py")).as_deref(),
            Some("/lib/u.py")
        );
        assert_eq!(
            local_path(&abs_local, root, "/lib/u.py"),
            Some(PathBuf::from("/shared/lib/u.py"))
        );

        let backslashes = server().with_mapping("src\\pkg", "pkg");
        assert_eq!(
            remote_path(&backslashes, root, Path::new("/p/src/pkg/m.py")).as_deref(),
            Some("/var/www/app/pkg/m.py")
        );
    }

    #[test]
    fn round_trip() {
        let root = Path::new("/home/me/proj");
        let s = server()
            .with_mapping("", "")
            .with_mapping("web/static", "static");
        for rel in [
            "a.py",
            "pkg/sub/m.py",
            "web/static/x y.css",
            "web/index.html",
            "it's here.txt",
        ] {
            let local = root.join(rel);
            let remote = remote_path(&s, root, &local).unwrap();
            assert_eq!(
                local_path(&s, root, &remote),
                Some(local),
                "{rel} -> {remote}"
            );
        }
    }

    #[test]
    fn web_urls() {
        let root = Path::new("/p");
        let s = server().with_web_url("https://example.com/");
        assert_eq!(
            web_url(&s, root, Path::new("/p/docs/a b.html")).as_deref(),
            Some("https://example.com/docs/a%20b.html")
        );
        assert_eq!(
            web_url(&s, root, root).as_deref(),
            Some("https://example.com/")
        );
        assert_eq!(
            web_url(&server(), root, Path::new("/p/x.html")),
            None,
            "no web URL configured"
        );
        let outside = server().with_mapping("", "/").with_web_url("http://h");
        assert_eq!(
            web_url(&outside, root, Path::new("/p/x.html")).as_deref(),
            Some("http://h/x.html")
        );
    }

    #[test]
    fn percent_encoding() {
        assert_eq!(
            percent_encode_segment("a b#c?d%e;f[g]"),
            "a%20b%23c%3Fd%25e%3Bf%5Bg%5D"
        );
        assert_eq!(percent_encode_segment("it's"), "it%27s");
        assert_eq!(percent_encode_segment("é"), "%C3%A9");
        assert_eq!(percent_encode_segment("A-z_0.9~"), "A-z_0.9~");
    }
}
#[cfg(test)]
mod web_mapping_fixtures {
    use super::*;
    #[test]
    fn local_deployed_and_web_roots_are_independent() {
        let mut server =
            Server::sftp("prod", "host", "user", "/srv").with_web_url("https://example.org");
        let mut mapping = Mapping::new("src", "application");
        mapping.web_path = "static".into();
        server.mappings.push(mapping);
        assert_eq!(
            remote_path(
                &server,
                Path::new("/project"),
                Path::new("/project/src/a b.py")
            )
            .as_deref(),
            Some("/srv/application/a b.py")
        );
        assert_eq!(
            web_url(
                &server,
                Path::new("/project"),
                Path::new("/project/src/a b.py")
            )
            .as_deref(),
            Some("https://example.org/static/a%20b.py")
        );
    }
}

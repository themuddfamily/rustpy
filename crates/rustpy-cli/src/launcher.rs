//! `rustpy install-launcher` and Tools › Create Command-line Launcher…: a small
//! script on the `PATH` that starts this RustPy binary.

use std::path::{Path, PathBuf};

/// Every launcher script carries this line, so it can be replaced safely.
pub const MARKER: &str = "RustPy command-line launcher";

fn script_name() -> &'static str {
    if cfg!(windows) {
        "rustpy.cmd"
    } else {
        "rustpy"
    }
}

/// `~/.local/bin/rustpy` (Linux, macOS), `%LOCALAPPDATA%\Programs\RustPy\bin\rustpy.cmd` (Windows).
pub fn default_path() -> Option<PathBuf> {
    if cfg!(windows) {
        dirs::data_local_dir().map(|d| {
            d.join("Programs")
                .join("RustPy")
                .join("bin")
                .join(script_name())
        })
    } else {
        dirs::home_dir().map(|h| h.join(".local").join("bin").join(script_name()))
    }
}

/// Where the script goes: an existing folder (or a path ending in a separator)
/// receives `rustpy`; any other path is the script itself.
pub fn script_path(given: Option<&Path>) -> Result<PathBuf, String> {
    let Some(p) = given else {
        return default_path()
            .ok_or_else(|| "no home directory; give the launcher's path".to_string());
    };
    let s = p.to_string_lossy();
    if p.is_dir() || s.ends_with('/') || s.ends_with(std::path::MAIN_SEPARATOR) {
        Ok(p.join(script_name()))
    } else {
        Ok(p.to_path_buf())
    }
}

/// The program the launcher starts: the AppImage when running from one (its
/// mount point is temporary), otherwise this executable.
pub fn target_executable() -> Result<PathBuf, String> {
    if let Some(appimage) = std::env::var_os("APPIMAGE")
        .filter(|a| !a.is_empty())
        .map(PathBuf::from)
    {
        if appimage.is_file() {
            return Ok(appimage);
        }
    }
    let exe =
        std::env::current_exe().map_err(|e| format!("cannot find the RustPy executable: {e}"))?;
    Ok(exe.canonicalize().unwrap_or(exe))
}

/// `'…'` for POSIX shells.
fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// The launcher script for `exe`.
pub fn script_text(exe: &Path) -> String {
    let exe = exe.to_string_lossy();
    if cfg!(windows) {
        // `%` would start a variable in a batch file.
        format!("@echo off\r\nrem {MARKER} (rustpy install-launcher, Tools > Create Command-line Launcher...)\r\n\"{}\" %*\r\n", exe.replace('%', "%%"))
    } else {
        format!("#!/bin/sh\n# {MARKER} (rustpy install-launcher, Tools > Create Command-line Launcher...)\nexec {} \"$@\"\n", sh_quote(&exe))
    }
}

/// What [`install`] did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Installed {
    pub path: PathBuf,
    pub target: PathBuf,
    /// An older launcher script was replaced.
    pub replaced: bool,
    /// `path` already is the RustPy executable (scripts/install.sh puts the
    /// binary at `~/.local/bin/rustpy`): nothing was written.
    pub already_installed: bool,
    /// The script's folder is on the `PATH`.
    pub on_path: bool,
}

impl Installed {
    /// A message for the user.
    pub fn describe(&self) -> String {
        let mut msg = if self.already_installed {
            format!(
                "{} already is the RustPy executable; nothing to do.",
                self.path.display()
            )
        } else if self.replaced {
            format!(
                "Updated the launcher {} (starts {}).",
                self.path.display(),
                self.target.display()
            )
        } else {
            format!(
                "Created the launcher {} (starts {}).",
                self.path.display(),
                self.target.display()
            )
        };
        if !self.on_path {
            let dir = self
                .path
                .parent()
                .unwrap_or(Path::new(""))
                .display()
                .to_string();
            if cfg!(windows) {
                msg.push_str(&format!("\n{dir} is not on your PATH: add it in System Properties › Environment Variables to run 'rustpy' from a terminal."));
            } else {
                msg.push_str(&format!("\n{dir} is not on your PATH: add it (for example export PATH=\"{dir}:$PATH\" in ~/.profile) to run 'rustpy' from a terminal."));
            }
        }
        msg
    }
}

fn same_file(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(x), Ok(y)) if x == y => return true,
        _ => {}
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if let (Ok(x), Ok(y)) = (std::fs::metadata(a), std::fs::metadata(b)) {
            return x.dev() == y.dev() && x.ino() == y.ino();
        }
    }
    false
}

fn dir_on_path(dir: &Path) -> bool {
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    let dir_c = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
    std::env::split_paths(&path).any(|p| p == dir || p.canonicalize().is_ok_and(|c| c == dir_c))
}

/// Writes the launcher at `given` (see [`script_path`]) for `target` (see
/// [`target_executable`]). A file that is not a launcher script is only
/// replaced with `force`; the RustPy executable itself never is.
pub fn install_for(given: Option<&Path>, target: &Path, force: bool) -> Result<Installed, String> {
    let path = script_path(given)?;
    let on_path = path.parent().is_some_and(dir_on_path);
    if path.is_dir() {
        return Err(format!("{} is a folder", path.display()));
    }
    let mut replaced = false;
    if path.exists() {
        if same_file(&path, target) {
            return Ok(Installed {
                path,
                target: target.to_path_buf(),
                replaced: false,
                already_installed: true,
                on_path,
            });
        }
        let is_launcher = std::fs::read(&path)
            .map(|b| String::from_utf8_lossy(&b[..b.len().min(4096)]).contains(MARKER))
            .unwrap_or(false);
        if !is_launcher && !force {
            return Err(format!(
                "{} exists and is not a RustPy launcher; use --force to replace it",
                path.display()
            ));
        }
        replaced = true;
    }
    let dir = path
        .parent()
        .filter(|d| !d.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let tmp = dir.join(format!(".{}.{}.tmp", script_name(), std::process::id()));
    std::fs::write(&tmp, script_text(target))
        .map_err(|e| format!("cannot write {}: {e}", tmp.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Err(e) = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755)) {
            let _ = std::fs::remove_file(&tmp);
            return Err(format!("cannot make {} executable: {e}", tmp.display()));
        }
    }
    if let Err(e) = std::fs::rename(&tmp, &path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("cannot write {}: {e}", path.display()));
    }
    Ok(Installed {
        path,
        target: target.to_path_buf(),
        replaced,
        already_installed: false,
        on_path,
    })
}

/// [`install_for`] this RustPy.
pub fn install(given: Option<&Path>, force: bool) -> Result<Installed, String> {
    install_for(given, &target_executable()?, force)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scripts_quote_the_executable() {
        let text = script_text(Path::new("/opt/it's here/rustpy"));
        assert!(text.contains(MARKER));
        if cfg!(unix) {
            assert!(text.starts_with("#!/bin/sh\n"));
            assert!(
                text.contains(r#"exec '/opt/it'\''s here/rustpy' "$@""#),
                "{text}"
            );
        }
    }

    #[test]
    fn paths_for_folders_and_files() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            script_path(Some(dir.path())).unwrap(),
            dir.path().join(script_name())
        );
        assert_eq!(
            script_path(Some(&dir.path().join("rpy"))).unwrap(),
            dir.path().join("rpy")
        );
        let trailing = format!(
            "{}{}",
            dir.path().join("new").display(),
            std::path::MAIN_SEPARATOR
        );
        assert_eq!(
            script_path(Some(Path::new(&trailing))).unwrap(),
            dir.path().join("new").join(script_name())
        );
        if let Some(p) = default_path() {
            assert!(
                p.ends_with(if cfg!(windows) {
                    "bin/rustpy.cmd"
                } else {
                    ".local/bin/rustpy"
                }),
                "{}",
                p.display()
            );
        }
    }

    #[test]
    fn installs_replaces_and_refuses() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("rustpy-bin");
        std::fs::write(&target, "binary").unwrap();
        let bin = dir.path().join("bin");
        let first = install_for(Some(&bin.join("")), &target, false).unwrap();
        assert_eq!(first.path, bin.join(script_name()));
        assert!(!first.replaced && !first.already_installed);
        let text = std::fs::read_to_string(&first.path).unwrap();
        assert!(text.contains(&target.display().to_string()), "{text}");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&first.path).unwrap().permissions().mode() & 0o777,
                0o755
            );
            let out = std::process::Command::new(&first.path)
                .arg("--version")
                .output();
            assert!(
                out.is_err() || !out.unwrap().status.success(),
                "the fake target is not runnable"
            );
        }
        let again = install_for(Some(&bin), &target, false).unwrap();
        assert!(again.replaced);
        assert!(first.describe().starts_with("Created the launcher"));
        assert!(again.describe().starts_with("Updated the launcher"));
        // Somebody else's program is kept unless --force.
        let other = bin.join("other");
        std::fs::write(&other, "#!/bin/sh\necho hi\n").unwrap();
        assert!(install_for(Some(&other), &target, false)
            .unwrap_err()
            .contains("--force"));
        assert!(install_for(Some(&other), &target, true).unwrap().replaced);
        // The binary itself (scripts/install.sh) is never overwritten.
        let same = install_for(Some(&target), &target, true).unwrap();
        assert!(same.already_installed);
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "binary");
        assert!(same.describe().contains("nothing to do"));
        assert!(install_for(Some(dir.path()), &target, false)
            .unwrap()
            .path
            .ends_with(script_name()));
    }
}

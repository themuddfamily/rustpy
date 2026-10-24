//! Starting a Python interpreter by its configured path.
//!
//! On Windows a remote interpreter (WSL, SSH, Docker, Compose, Vagrant) is
//! represented by a generated `remote-<sha256>.cmd` wrapper whose only
//! command is `"<local python>" -I "<adapter.py>" %*`. RustPy never spawns
//! that batch file itself: cmd re-parses `%*`, so arbitrary arguments (a
//! multi-line `-c` probe, user script arguments with quotes or `%`) are
//! either refused by the standard library (CVE-2024-24576 hardening) or, worse,
//! interpreted by cmd. [`interpreter_command`] recognises exactly the wrapper
//! format RustPy writes and starts the local adapter Python directly, so the
//! arguments added afterwards reach Python's `sys.argv` with ordinary
//! `CreateProcess` quoting. Every other path is started as given; other batch
//! files are never parsed or guessed at.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The direct launch behind a RustPy remote-interpreter wrapper.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteWrapperLaunch {
    /// The local Python that runs the transport adapter.
    pub python: PathBuf,
    /// The generated adapter script next to the wrapper.
    pub adapter: PathBuf,
}

impl RemoteWrapperLaunch {
    /// Leading arguments that precede the caller's arguments.
    pub fn leading_args(&self) -> Vec<OsString> {
        vec!["-I".into(), self.adapter.clone().into_os_string()]
    }
}

/// The key of a wrapper file name `remote-<64 lowercase hex>.cmd`.
pub fn remote_wrapper_key(file_name: &str) -> Option<&str> {
    if file_name.len() != "remote-".len() + 64 + ".cmd".len() || !file_name.is_ascii() {
        return None;
    }
    let (prefix, rest) = file_name.split_at("remote-".len());
    let (key, extension) = rest.split_at(64);
    (prefix.eq_ignore_ascii_case("remote-")
        && extension.eq_ignore_ascii_case(".cmd")
        && key.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')))
    .then_some(key)
}

/// Splits a Windows-style path text at its last `\` or `/`.
fn split_windows(text: &str) -> Option<(&str, &str)> {
    let index = text.rfind(['\\', '/'])?;
    Some((&text[..index], &text[index + 1..]))
}

fn same_windows_dir(a: &str, b: &str) -> bool {
    let normal = |s: &str| {
        s.trim_end_matches(['\\', '/'])
            .replace('/', "\\")
            .to_ascii_lowercase()
    };
    normal(a) == normal(b)
}

fn windows_absolute(text: &str) -> bool {
    let bytes = text.as_bytes();
    (bytes.len() > 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && matches!(bytes[2], b'\\' | b'/'))
        || text.starts_with("\\\\")
}

/// Reverses the generator's batch quoting: `"..."` with `%` doubled.
fn batch_unquote(word: &str) -> Option<String> {
    let inner = word.strip_prefix('"')?.strip_suffix('"')?;
    if inner.is_empty() || inner.contains(['"', '\r', '\n']) {
        return None;
    }
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        if c == '%' && chars.next() != Some('%') {
            return None;
        }
        out.push(c);
    }
    Some(out)
}

/// Recognises the exact wrapper written by RustPy's Windows remote
/// interpreter installer. `wrapper` is the wrapper's full path text and
/// `contents` its file contents. Pure, so it is testable on every platform.
///
/// Accepted: a file named `remote-<sha256>.cmd` containing exactly
/// `@echo off` and `"<abs python .exe>" -I "<same dir>\remote-<sha256>.py" %*`
/// (CRLF or LF line ends). Anything else returns `None`.
pub fn parse_remote_wrapper(wrapper: &str, contents: &str) -> Option<RemoteWrapperLaunch> {
    let (directory, name) = split_windows(wrapper)?;
    let key = remote_wrapper_key(name)?;
    let normalized = contents.replace("\r\n", "\n");
    let mut lines = normalized
        .strip_suffix('\n')
        .unwrap_or(&normalized)
        .split('\n');
    if lines.next()? != "@echo off" {
        return None;
    }
    let command = lines.next()?;
    if lines.next().is_some() {
        return None;
    }
    let rest = command.strip_suffix(" %*")?;
    // The two quoted words cannot contain quotes, so the separator is unique.
    let (python, adapter) = rest.split_once("\" -I \"")?;
    let python = batch_unquote(&format!("{python}\""))?;
    let adapter = batch_unquote(&format!("\"{adapter}"))?;
    if !windows_absolute(&python) || !windows_absolute(&adapter) {
        return None;
    }
    let (_, python_name) = split_windows(&python)?;
    if python_name.len() < 5 || !python_name[python_name.len() - 4..].eq_ignore_ascii_case(".exe") {
        return None;
    }
    let (adapter_dir, adapter_name) = split_windows(&adapter)?;
    if !same_windows_dir(adapter_dir, directory)
        || !adapter_name.eq_ignore_ascii_case(&format!("remote-{key}.py"))
    {
        return None;
    }
    Some(RemoteWrapperLaunch {
        python: PathBuf::from(python),
        adapter: PathBuf::from(adapter),
    })
}

/// True when `path` names a Windows batch file (`.cmd` or `.bat`).
pub fn is_batch_file(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("cmd") || e.eq_ignore_ascii_case("bat"))
}

/// Reads and recognises a RustPy remote wrapper on disk. Only files with the
/// generated name are read; the adapter script must exist next to it.
pub fn read_remote_wrapper(path: &Path) -> Option<RemoteWrapperLaunch> {
    let name = path.file_name()?.to_str()?;
    remote_wrapper_key(name)?;
    let metadata = std::fs::metadata(path).ok()?;
    if !metadata.is_file() || metadata.len() > 64 * 1024 {
        return None;
    }
    let contents = std::fs::read_to_string(path).ok()?;
    let launch = parse_remote_wrapper(path.to_str()?, &contents)?;
    launch.adapter.is_file().then_some(launch)
}

/// Program and leading arguments that start the interpreter at `path`.
/// Callers append their own arguments after the leading ones.
pub fn interpreter_invocation(path: &Path) -> (PathBuf, Vec<OsString>) {
    if cfg!(windows) && is_batch_file(path) {
        if let Some(launch) = read_remote_wrapper(path) {
            let args = launch.leading_args();
            return (launch.python, args);
        }
    }
    (path.to_path_buf(), Vec::new())
}

/// Like [`interpreter_command`], but refuses a Windows batch file that is not
/// a RustPy remote wrapper instead of handing it to cmd.
pub fn try_interpreter_command(path: &Path) -> std::io::Result<Command> {
    if cfg!(windows) && is_batch_file(path) && read_remote_wrapper(path).is_none() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "{} is a batch file, not a Python interpreter or RustPy remote interpreter wrapper",
                path.display()
            ),
        ));
    }
    Ok(interpreter_command(path))
}

/// A [`Command`] that starts the Python interpreter configured at `path`.
/// Arguments added with `.arg(s)` go to Python's `sys.argv` unchanged. On
/// Windows a RustPy remote wrapper is started through its adapter Python
/// directly, never through cmd.
pub fn interpreter_command(path: &Path) -> Command {
    let (program, args) = interpreter_invocation(path);
    let mut command = Command::new(program);
    command.args(args);
    command
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &str = "7e83a0b1c2d3e4f5a6b7c8d9e0f1a2b3c4d5e6f7a8b9c0d1e2f3a4b5c6d7e8f9";

    fn wrapper() -> String {
        format!("C:\\Users\\Ann Lee\\AppData\\RustPy\\adapters\\remote-{KEY}.cmd")
    }

    fn generated(python: &str, adapter: &str) -> String {
        format!("@echo off\r\n\"{python}\" -I \"{adapter}\" %*\r\n")
    }

    #[test]
    fn generated_wrapper_is_recognised_with_percent_unescaped() {
        let adapter = format!("C:\\Users\\Ann Lee\\AppData\\RustPy\\adapters\\remote-{KEY}.py");
        let python = "C:\\Program Files\\Python 100%%\\python.exe";
        let launch = parse_remote_wrapper(&wrapper(), &generated(python, &adapter)).unwrap();
        assert_eq!(
            launch.python,
            PathBuf::from("C:\\Program Files\\Python 100%\\python.exe")
        );
        assert_eq!(launch.adapter, PathBuf::from(&adapter));
        assert_eq!(
            launch.leading_args(),
            vec![OsString::from("-I"), OsString::from(&adapter)]
        );
        // LF line ends (e.g. a checkout) and a mixed-case directory match too.
        let lf = format!("@echo off\n\"C:\\py\\python3.exe\" -I \"c:/users/ann lee/appdata/rustpy/adapters/remote-{KEY}.py\" %*\n");
        assert!(parse_remote_wrapper(&wrapper(), &lf).is_some());
    }

    #[test]
    fn anything_but_the_generated_wrapper_is_refused() {
        let adapter = format!("C:\\Users\\Ann Lee\\AppData\\RustPy\\adapters\\remote-{KEY}.py");
        let ok = generated("C:\\py\\python.exe", &adapter);
        assert!(parse_remote_wrapper(&wrapper(), &ok).is_some());
        let refused = [
            // wrong file names
            (format!("C:\\a\\remote-{KEY}.bat"), ok.clone()),
            ("C:\\a\\remote-abc.cmd".to_string(), ok.clone()),
            (
                format!("C:\\a\\remote-{}.cmd", KEY.to_uppercase()),
                ok.clone(),
            ),
            (format!("C:\\a\\python-{KEY}.cmd"), ok.clone()),
            // adapter in another directory or for another key
            (
                wrapper(),
                generated("C:\\py\\python.exe", &format!("C:\\other\\remote-{KEY}.py")),
            ),
            (
                wrapper(),
                generated("C:\\py\\python.exe", &adapter.replace("7e83", "0000")),
            ),
            // relative or non-exe python
            (wrapper(), generated("python.exe", &adapter)),
            (wrapper(), generated("C:\\py\\python.cmd", &adapter)),
            // unescaped percent (cmd expansion), extra commands, missing %*
            (wrapper(), generated("C:\\py\\%PATH%\\python.exe", &adapter)),
            (wrapper(), format!("{ok}del C:\\x\r\n")),
            (wrapper(), ok.replace(" %*", "")),
            (wrapper(), ok.replace("@echo off", "@echo on")),
            (wrapper(), ok.replace("\" -I \"", "\" -I -c \"")),
            (wrapper(), String::new()),
        ];
        for (path, contents) in refused {
            assert_eq!(
                parse_remote_wrapper(&path, &contents),
                None,
                "{path}: {contents:?}"
            );
        }
    }

    #[test]
    fn normal_paths_start_directly() {
        for path in [
            "/usr/bin/python3",
            "C:\\Python312\\python.exe",
            "relative/python",
        ] {
            let command = interpreter_command(Path::new(path));
            assert_eq!(command.get_program(), path);
            assert_eq!(command.get_args().count(), 0);
            assert!(try_interpreter_command(Path::new(path)).is_ok());
        }
        let mut command = interpreter_command(Path::new("/usr/bin/python3"));
        command.args(["-I", "-c", "print(1)\nprint(2)"]);
        assert_eq!(command.get_args().count(), 3);
    }

    #[cfg(not(windows))]
    #[test]
    fn wrapper_files_are_only_rewritten_on_windows() {
        let directory = std::env::temp_dir().join(format!("rustpy-launch-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let wrapper = directory.join(format!("remote-{KEY}.cmd"));
        std::fs::write(&wrapper, "@echo off\r\n").unwrap();
        assert_eq!(
            interpreter_command(&wrapper).get_program(),
            wrapper.as_os_str()
        );
        assert!(try_interpreter_command(&wrapper).is_ok());
        let _ = std::fs::remove_dir_all(&directory);
    }

    #[cfg(windows)]
    #[test]
    fn windows_wrapper_starts_the_adapter_python_directly() {
        let directory = std::env::temp_dir().join(format!("rustpy-launch-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let wrapper = directory.join(format!("remote-{KEY}.cmd"));
        let adapter = directory.join(format!("remote-{KEY}.py"));
        std::fs::write(&adapter, "import sys\n").unwrap();
        let python = std::env::current_exe().unwrap();
        std::fs::write(
            &wrapper,
            generated(python.to_str().unwrap(), adapter.to_str().unwrap()),
        )
        .unwrap();
        let mut command = interpreter_command(&wrapper);
        command.args(["-c", "a\nb \"q\" %PATH%"]);
        assert_eq!(command.get_program(), python.as_os_str());
        let args: Vec<_> = command.get_args().collect();
        assert_eq!(args[0], "-I");
        assert_eq!(args[1], adapter.as_os_str());
        assert_eq!(args[3], "a\nb \"q\" %PATH%");
        let other = directory.join("other.cmd");
        std::fs::write(&other, "@echo off\r\n").unwrap();
        assert!(try_interpreter_command(&other).is_err());
        let _ = std::fs::remove_dir_all(&directory);
    }
}

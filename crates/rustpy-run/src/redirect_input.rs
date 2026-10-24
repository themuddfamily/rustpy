//! Headless execution host: keep stdout/stderr on the PTY and give stdin the
//! actual file, including EOF and binary data. No shell parses user arguments.
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::OnceLock;

const FLAG: &str = "--rustpy-internal-redirect-stdin";
static HOST: OnceLock<PathBuf> = OnceLock::new();

/// The desktop entrypoint registers itself before creating any run sessions.
pub fn register_host(executable: PathBuf) {
    let _ = HOST.set(executable);
}

pub(crate) fn command(
    program: &Path,
    arguments: &[String],
    input: &Path,
) -> anyhow::Result<(PathBuf, Vec<String>)> {
    let host = HOST.get().ok_or_else(|| anyhow::anyhow!(
        "The run host must register its executable before combining terminal emulation with input redirection"
    ))?;
    let mut args = vec![
        FLAG.into(),
        input
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("Input path is not valid Unicode"))?
            .into(),
        program
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("Program path is not valid Unicode"))?
            .into(),
    ];
    args.extend_from_slice(arguments);
    Ok((host.clone(), args))
}

/// Must run before CLI parsing, logging, askpass dispatch or GUI initialization.
/// Returns None for every ordinary launcher invocation.
pub fn helper_main() -> Option<i32> {
    let mut args = std::env::args_os().skip(1);
    if args.next().as_deref() != Some(std::ffi::OsStr::new(FLAG)) {
        return None;
    }
    Some(execute(args))
}
fn execute(mut args: impl Iterator<Item = OsString>) -> i32 {
    let (Some(input), Some(program)) = (args.next(), args.next()) else {
        eprintln!("RustPy input redirection: expected input file and program");
        return 126;
    };
    let input = match std::fs::File::open(&input) {
        Ok(input) => input,
        Err(error) => {
            eprintln!("RustPy input redirection: {error}");
            return 126;
        }
    };
    let mut command = rustpy_project::interpreter::interpreter_command(Path::new(&program));
    command
        .args(args)
        .stdin(Stdio::from(input))
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let error = command.exec();
        eprintln!("RustPy input redirection: {error}");
        126
    }
    #[cfg(not(unix))]
    {
        match command.status() {
            Ok(status) => status.code().unwrap_or(1),
            Err(error) => {
                eprintln!("RustPy input redirection: {error}");
                126
            }
        }
    }
}

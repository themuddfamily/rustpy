//! Logging for every mode: `<log-dir>/rustpy.log` (rotated during writing at 5 MiB, three old files kept) plus stderr for IDE windows.

use std::io::Write;

/// Where log records go besides the log file.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Mode {
    /// The IDE and the diff/merge windows: also stderr.
    Window,
    /// Headless commands (`format`, `inspect`): stderr only with `--verbose`,
    /// so their own output stays readable.
    Headless,
    /// Stdio protocol servers: the file, and stderr only with --verbose.
    Protocol,
}

const FILE_BYTES: u64 = 5 * 1024 * 1024;

/// The writer has closed its file before rotation (also works on Windows).
fn rotate(dir: &std::path::Path) {
    let current = dir.join("rustpy.log");
    let _ = std::fs::remove_file(dir.join("rustpy.3.log"));
    for i in (1..3).rev() {
        let _ = std::fs::rename(
            dir.join(format!("rustpy.{i}.log")),
            dir.join(format!("rustpy.{}.log", i + 1)),
        );
    }
    let _ = std::fs::rename(current, dir.join("rustpy.1.log"));
}

struct Tee {
    dir: std::path::PathBuf,
    file: Option<std::fs::File>,
    bytes: u64,
    stderr: bool,
}

fn open_log(dir: &std::path::Path) -> std::io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.custom_flags(0x0020_0000); // FILE_FLAG_OPEN_REPARSE_POINT
    }
    let file = options.open(dir.join("rustpy.log"))?;
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if file.metadata()?.file_attributes() & 0x400 != 0 {
            return Err(std::io::Error::other("log reparse points are excluded"));
        }
    }
    if !file.metadata()?.is_file() {
        return Err(std::io::Error::other(
            "log destination is not a regular file",
        ));
    }
    Ok(file)
}

impl Tee {
    fn new(dir: std::path::PathBuf, stderr: bool) -> Self {
        let _ = std::fs::create_dir_all(&dir);
        if std::fs::symlink_metadata(dir.join("rustpy.log"))
            .map(|m| m.is_file() && m.len() >= FILE_BYTES)
            .unwrap_or(false)
        {
            rotate(&dir);
        }
        let file = open_log(&dir).ok();
        let bytes = file
            .as_ref()
            .and_then(|f| f.metadata().ok())
            .map_or(0, |m| m.len());
        Self {
            dir,
            file,
            bytes,
            stderr,
        }
    }
}

impl Write for Tee {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if self.stderr {
            let _ = std::io::stderr().write_all(buf);
        }
        let mut pending = buf;
        while !pending.is_empty() && self.file.is_some() {
            if self.bytes >= FILE_BYTES {
                self.file = None;
                rotate(&self.dir);
                self.file = open_log(&self.dir).ok();
                self.bytes = self
                    .file
                    .as_ref()
                    .and_then(|f| f.metadata().ok())
                    .map_or(0, |m| m.len());
                // Failed rotation must not loop forever or grow a full file.
                if self.bytes >= FILE_BYTES || self.file.is_none() {
                    self.file = None;
                    break;
                }
            }
            let length = pending.len().min((FILE_BYTES - self.bytes) as usize);
            if self
                .file
                .as_mut()
                .unwrap()
                .write_all(&pending[..length])
                .is_err()
            {
                self.file = None;
                break;
            }
            self.bytes += length as u64;
            pending = &pending[length..];
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        if let Some(f) = &mut self.file {
            f.flush()?;
        }
        std::io::stderr().flush()
    }
}

/// `RUST_LOG` (or the default) merged with Help › Diagnostic Tools › Debug Log
/// Settings: a record is written when either admits it, so the categories can
/// change at runtime while `RUST_LOG` stays in force.
struct Dynamic {
    /// Only its filter is used.
    filter: env_logger::Logger,
    /// Formats and writes every record handed to it.
    sink: env_logger::Logger,
}

impl Dynamic {
    fn admits(&self, metadata: &log::Metadata<'_>) -> bool {
        log::Log::enabled(&self.filter, metadata)
            || rustpy_settings::debug_log::enabled(metadata.target(), metadata.level())
    }
}

impl log::Log for Dynamic {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        self.admits(metadata)
    }

    fn log(&self, record: &log::Record<'_>) {
        // The filter's own check also applies its optional message regex.
        if self.filter.matches(record)
            || rustpy_settings::debug_log::enabled(record.target(), record.level())
        {
            self.sink.log(record);
        }
    }

    fn flush(&self) {
        self.sink.flush();
    }
}

/// Installs the logger (`RUST_LOG` filters; the default is `info`, or `debug`
/// with `--verbose`; Debug Log Settings categories add to it at runtime).
/// Call once, after the global options are applied.
pub fn init(verbose: bool, mode: Mode) {
    let dir = rustpy_settings::Settings::log_dir();
    let stderr = mode == Mode::Window || verbose;
    let context = match mode {
        Mode::Window => "window",
        Mode::Headless => "headless",
        Mode::Protocol => "lsp",
    };
    crate::diagnostics::set_runtime_mode(context);
    // D-Bus (zbus: accessibility, the Secret Service keyring) logs every handshake at info.
    let filter =
        env_logger::Builder::from_env(env_logger::Env::default().default_filter_or(if verbose {
            "debug"
        } else {
            "info,zbus=warn,tracing=warn"
        }))
        .build();
    let base = filter.filter();
    let mut sink = env_logger::Builder::new();
    if let Ok(style) = std::env::var("RUST_LOG_STYLE") {
        sink.parse_write_style(&style);
    }
    let sink = sink
        .filter_level(log::LevelFilter::Trace)
        .target(env_logger::Target::Pipe(Box::new(Tee::new(dir, stderr))))
        .build();
    if log::set_boxed_logger(Box::new(Dynamic { filter, sink })).is_ok() {
        rustpy_settings::debug_log::set_base_level(base);
        log::info!(
            "RustPy {} started (mode={context}, os={}, arch={}, pid={})",
            crate::help::VERSION,
            std::env::consts::OS,
            std::env::consts::ARCH,
            std::process::id()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_running_writes_rotate_and_keep_three_old_files() {
        let dir = tempfile::tempdir().unwrap();
        let mut tee = Tee::new(dir.path().into(), false);
        for byte in b'a'..=b'e' {
            tee.write_all(&vec![byte; FILE_BYTES as usize]).unwrap();
        }
        tee.flush().unwrap();
        for (name, byte) in [
            ("rustpy.log", b'e'),
            ("rustpy.1.log", b'd'),
            ("rustpy.2.log", b'c'),
            ("rustpy.3.log", b'b'),
        ] {
            let contents = std::fs::read(dir.path().join(name)).unwrap();
            assert_eq!(contents.len(), FILE_BYTES as usize);
            assert!(contents.iter().all(|&b| b == byte));
        }
        assert!(!dir.path().join("rustpy.4.log").exists());
    }

    #[test]
    fn debug_log_categories_add_records_beside_the_rust_log_filter() {
        let filter = env_logger::Builder::new().parse_filters("info").build();
        let sink = env_logger::Builder::new()
            .filter_level(log::LevelFilter::Trace)
            .build();
        let logger = Dynamic { filter, sink };
        let meta = |target: &'static str, level| {
            log::Metadata::builder().target(target).level(level).build()
        };
        rustpy_settings::debug_log::apply("rustpy_cli_logging_probe:trace").unwrap();
        assert!(logger.admits(&meta("rustpy_cli_logging_probe::x", log::Level::Trace)));
        // RUST_LOG still admits what it always did; others stay filtered.
        assert!(logger.admits(&meta("other", log::Level::Info)));
        assert!(!logger.admits(&meta("other", log::Level::Debug)));
        rustpy_settings::debug_log::apply("").unwrap();
        assert!(!logger.admits(&meta("rustpy_cli_logging_probe", log::Level::Debug)));
    }

    #[test]
    fn logging_failure_never_breaks_the_command_output() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("not-a-directory");
        std::fs::write(&path, "keep me").unwrap();
        let mut tee = Tee::new(path.clone(), false);
        assert_eq!(tee.write(b"still running").unwrap(), 13);
        tee.flush().unwrap();
        assert_eq!(std::fs::read_to_string(path).unwrap(), "keep me");
    }
}

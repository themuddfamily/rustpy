//! RustPy command-line launcher (roadmap §8 "Command-line launcher"):
//! argument parsing for every `rustpy` command, the single-instance IPC
//! that forwards `rustpy path…` to a running IDE, and the commands that run
//! without a window (`format`, `inspect`, `completions`, `install-launcher`,
//! `help`, `--version`). The windows (`rustpy [path…]`, `diff`, `merge`) are
//! started by `rustpy_ui`.

pub mod args;
pub mod completions;
pub mod diagnostics;
pub mod format;
pub mod help;
mod http;
pub mod inspect;
pub mod instance;
pub mod ipc;
pub mod launcher;
pub mod logging;
mod mini_xml;
pub mod profile;
pub mod quit_coordinator;
pub mod report;
pub mod style;
pub mod test;
pub mod window_admission;
pub mod window_registry;
pub mod window_sessions;

use std::io::Write;
use std::path::{Path, PathBuf};

pub use args::{parse, ArgError, Cli, Command, GlobalOptions};

fn absolute(p: &Path) -> PathBuf {
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|d| d.join(p))
            .unwrap_or_else(|_| p.to_path_buf())
    }
}

/// Applies `--config-dir`, `--system-dir`, `--log-dir` and `--disable-plugins`
/// through their environment variables (`RUSTPY_CONFIG_DIR`, …), which the
/// whole IDE and the processes it starts (new windows) read. Call before any
/// thread starts.
pub fn apply_global_options(global: &GlobalOptions) {
    for (var, dir) in [
        ("RUSTPY_CONFIG_DIR", &global.config_dir),
        ("RUSTPY_SYSTEM_DIR", &global.system_dir),
        ("RUSTPY_LOG_DIR", &global.log_dir),
    ] {
        if let Some(d) = dir {
            std::env::set_var(var, absolute(d));
        }
    }
    if global.disable_plugins {
        std::env::set_var("RUSTPY_DISABLE_PLUGINS", "1");
    }
}

/// Lets a Windows release build (a GUI program) print to the console it was
/// started from, for the commands that print.
pub fn attach_parent_console() {
    #[cfg(windows)]
    {
        #[link(name = "kernel32")]
        extern "system" {
            fn AttachConsole(process_id: u32) -> i32;
        }
        const ATTACH_PARENT_PROCESS: u32 = u32::MAX;
        // SAFETY: no arguments besides a constant; failure (no console) is fine.
        unsafe {
            AttachConsole(ATTACH_PARENT_PROCESS);
        }
    }
}

/// Prints a command-line error with a pointer to the help; returns status 2.
pub fn report_arg_error(e: &ArgError, err: &mut dyn Write) -> i32 {
    let _ = writeln!(err, "rustpy: {e}");
    let hint = match e.command {
        Some(c) => format!("rustpy {c} -h"),
        None => "rustpy --help".to_string(),
    };
    let _ = writeln!(err, "Try '{hint}' for more information.");
    2
}

/// Whether `command` opens a window (run by `rustpy_ui`).
pub fn opens_window(command: &Command) -> bool {
    matches!(
        command,
        Command::Open(_) | Command::Diff(_) | Command::Merge(_)
    )
}

/// Runs a printing/headless command and returns its exit status; `None`
/// for windows and the stdio server, which the launcher runs directly.
pub fn run_headless(
    command: &Command,
    _global: &GlobalOptions,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Option<i32> {
    if _global.headless && opens_window(command) {
        let _ = writeln!(
            err,
            "rustpy: --headless forbids opening an IDE, diff or merge window"
        );
        return Some(2);
    }
    let code = match command {
        Command::Test(args) => test::run(args, &rustpy_settings::Settings::load(), out, err),
        Command::Open(_)
        | Command::Diff(_)
        | Command::Merge(_)
        | Command::Lsp
        | Command::RemoteServer { .. } => return None,
        Command::Help(topic) => {
            let _ = out.write_all(help::usage(topic.as_deref()).as_bytes());
            0
        }
        Command::Version => {
            let _ = out.write_all(help::version_text().as_bytes());
            if _global.verbose {
                let _ = out.write_all(help::version_details().as_bytes());
            }
            0
        }
        Command::Completions(shell) => {
            let _ = out.write_all(completions::script(*shell).as_bytes());
            0
        }
        Command::InstallLauncher(a) => match launcher::install(a.path.as_deref(), a.force) {
            Ok(done) => {
                let _ = writeln!(out, "{}", done.describe());
                0
            }
            Err(e) => {
                let _ = writeln!(err, "rustpy install-launcher: {e}");
                1
            }
        },
        Command::Diagnostics(output) => {
            match diagnostics::collect(&rustpy_settings::Settings::log_dir(), output.as_deref()) {
                Ok(report) => {
                    let _ = writeln!(
                        out,
                        "Diagnostic data saved to {} ({} files)",
                        report.path.display(),
                        report.collected_files
                    );
                    for issue in report.issues {
                        let _ =
                            writeln!(err, "rustpy diagnostics: {}: {}", issue.name, issue.reason);
                    }
                    0
                }
                Err(e) => {
                    let _ = writeln!(err, "rustpy diagnostics: {e}");
                    1
                }
            }
        }
        Command::Http(args) => http::run(args, out, err),
        Command::Format(a) => format::run(a, rustpy_settings::Settings::load(), out, err),
        Command::Inspect(a) => inspect::run(a, &rustpy_settings::Settings::load(), out, err),
    };
    Some(code)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headless(args: &[&str]) -> (Option<i32>, String, String) {
        let cli = parse(args.iter().copied()).unwrap();
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = run_headless(&cli.command, &cli.global, &mut out, &mut err);
        (
            code,
            String::from_utf8(out).unwrap(),
            String::from_utf8(err).unwrap(),
        )
    }

    #[test]
    fn help_version_and_completions_print() {
        let (code, out, _) = headless(&["--help"]);
        assert_eq!(code, Some(0));
        assert!(
            out.contains("Usage:") && out.contains("rustpy inspect"),
            "{out}"
        );
        let (_, out, _) = headless(&["format", "-h"]);
        assert!(out.contains("-allowDefaults"), "{out}");
        let (_, out, _) = headless(&["--version"]);
        assert_eq!(out, format!("RustPy {}\n", help::VERSION));
        let (code, out, _) = headless(&["--verbose", "--version"]);
        assert_eq!(code, Some(0));
        assert!(
            out.starts_with(&format!("RustPy {}\ninstall: ", help::VERSION))
                && out.contains("\nupdates: "),
            "{out}"
        );
        let (_, out, _) = headless(&["completions", "bash"]);
        assert!(out.contains("complete -o filenames"), "{out}");
        assert_eq!(headless(&["proj"]).0, None, "the IDE is not headless");
        assert_eq!(headless(&["diff", "a", "b"]).0, None);
    }

    #[test]
    fn argument_errors_point_at_the_help() {
        let e = parse(["format"]).unwrap_err();
        let mut err = Vec::new();
        assert_eq!(report_arg_error(&e, &mut err), 2);
        let text = String::from_utf8(err).unwrap();
        assert!(
            text.starts_with("rustpy: format needs") && text.contains("rustpy format -h"),
            "{text}"
        );
    }

    #[test]
    fn headless_guard_also_rejects_programmatically_constructed_window_commands() {
        let global = GlobalOptions {
            headless: true,
            ..Default::default()
        };
        let mut out = Vec::new();
        let mut err = Vec::new();
        assert_eq!(
            run_headless(
                &Command::Open(Default::default()),
                &global,
                &mut out,
                &mut err
            ),
            Some(2)
        );
        assert!(out.is_empty());
        assert!(String::from_utf8(err).unwrap().contains("forbids"));
    }

    #[test]
    fn relative_directories_become_absolute() {
        assert!(absolute(Path::new("cfg")).is_absolute());
        assert_eq!(absolute(Path::new("/x")), PathBuf::from("/x"));
    }
}

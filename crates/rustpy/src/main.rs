#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

//! The `rustpy` launcher: `rustpy [path …]` opens the IDE, `rustpy diff` /
//! `merge` open their windows, and `format`, `inspect`, `completions`,
//! `lsp`, `install-launcher`, `help` and `--version` run without a window (see
//! `rustpy_cli` and `rustpy --help`).

use std::process::ExitCode;

use rustpy_cli::{logging, Command};

fn main() -> ExitCode {
    // Covers every mode, including askpass and failures before CLI dispatch.
    // Reports are local and the hook never writes to protocol stdout.
    rustpy_cli::diagnostics::install_panic_hook();
    if let Some(code) = rustpy_ui::redirected_input_helper_main() {
        return exit(code);
    }
    if let Ok(executable) = std::env::current_exe() {
        rustpy_ui::register_run_host(executable);
    }
    // Started by git or ssh as the ask-pass program: answer the prompt through
    // the running IDE and exit, without logging or opening a window.
    if let Some(code) = rustpy_ui::askpass_helper_main() {
        return exit(code);
    }
    let cli = match rustpy_cli::parse(std::env::args().skip(1)) {
        Ok(cli) => cli,
        Err(e) => {
            rustpy_cli::attach_parent_console();
            return exit(rustpy_cli::report_arg_error(&e, &mut std::io::stderr()));
        }
    };
    rustpy_cli::apply_global_options(&cli.global);
    if cli.global.headless && rustpy_cli::opens_window(&cli.command) {
        rustpy_cli::attach_parent_console();
        eprintln!("rustpy: --headless forbids window-opening commands");
        return exit(2);
    }
    // Stdio belongs to the protocol; initialize logging only after global options.
    if let Command::RemoteServer { root, admit_client } = &cli.command {
        rustpy_cli::attach_parent_console();
        logging::init(cli.global.verbose, logging::Mode::Protocol);
        return match rustpy_lsp::gateway::run_stdio(root.clone(), *admit_client) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("rustpy remote-server: {error}");
                ExitCode::from(1)
            }
        };
    }
    if cli.command == Command::Lsp {
        rustpy_cli::attach_parent_console();
        logging::init(cli.global.verbose, logging::Mode::Protocol);
        return match rustpy_lsp::run_stdio() {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                log::error!("{e:#}");
                eprintln!("rustpy lsp: {e:#}");
                ExitCode::from(1)
            }
        };
    }
    if !rustpy_cli::opens_window(&cli.command) {
        rustpy_cli::attach_parent_console();
        logging::init(cli.global.verbose, logging::Mode::Headless);
        let code = rustpy_cli::run_headless(
            &cli.command,
            &cli.global,
            &mut std::io::stdout(),
            &mut std::io::stderr(),
        )
        .unwrap_or(2);
        return exit(code);
    }
    logging::init(cli.global.verbose, logging::Mode::Window);
    let result = match cli.command {
        Command::Diff(args) => rustpy_ui::merge_tool::run_diff(args),
        Command::Merge(args) => rustpy_ui::merge_tool::run_merge(args),
        Command::Open(args) => rustpy_ui::launch::run_ide(args, &cli.global),
        _ => Ok(0),
    };
    match result {
        Ok(code) => exit(code),
        Err(e) => {
            log::error!("{e:#}");
            eprintln!("rustpy: {e:#}");
            exit(2)
        }
    }
}

fn exit(code: i32) -> ExitCode {
    ExitCode::from(u8::try_from(code).unwrap_or(1))
}

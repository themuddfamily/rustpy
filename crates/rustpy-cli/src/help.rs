//! Usage texts for `rustpy --help` and `rustpy <command> -h`.

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

pub fn version_text() -> String {
    format!("RustPy {VERSION}\n")
}

/// `rustpy --verbose --version`: how this copy was installed and its target, one
/// `key: value` per line (`install: msix` for Microsoft Store / MSIX installs, whose
/// updates come from the Store).
pub fn version_details() -> String {
    let platform = rustpy_updates::Platform::current();
    version_details_for(
        platform.install,
        rustpy_updates::has_package_identity(),
        std::env::consts::OS,
        std::env::consts::ARCH,
    )
}

fn version_details_for(
    install: rustpy_updates::InstallKind,
    package_identity: bool,
    os: &str,
    arch: &str,
) -> String {
    let updates = if install.updates_managed_externally() {
        "Microsoft Store"
    } else if install.self_updates() {
        "RustPy"
    } else {
        "package manager"
    };
    format!(
        "install: {}\npackage-identity: {}\nupdates: {updates}\ntarget: {os}-{arch}\n",
        install.id(),
        if package_identity { "yes" } else { "no" },
    )
}

#[cfg(test)]
mod version_detail_tests {
    use super::*;
    use rustpy_updates::InstallKind;

    #[test]
    fn verbose_version_reports_the_install_kind_and_update_owner() {
        let store = version_details_for(InstallKind::Store, true, "windows", "x86_64");
        assert_eq!(
            store,
            "install: msix\npackage-identity: yes\nupdates: Microsoft Store\ntarget: windows-x86_64\n"
        );
        let msi = version_details_for(InstallKind::Msi, false, "windows", "aarch64");
        assert!(
            msi.starts_with("install: msi\npackage-identity: no\nupdates: RustPy\n"),
            "{msi}"
        );
        let snap = version_details_for(InstallKind::Snap, false, "linux", "x86_64");
        assert!(snap.contains("updates: package manager"), "{snap}");
        assert!(version_details().contains("package-identity: "));
    }
}

const MAIN: &str = "\
RustPy {version}: a Python IDE written in Rust

Usage:
  rustpy [options] [--line <n>] [--column <n>] [--wait] [<path> ...]
  rustpy diff <left> <right> [<base>]
  rustpy merge <left> <right> [<base>] <output>
  rustpy http <file.http> [--env name] [--request name-or-index]
              [--var key=value] [--timeout ms] [--json]
  rustpy format [options] <path> ...
  rustpy inspect <project> <profile> <output-dir> [options]
  rustpy test [options] [<target> ...] [-- <framework-argument> ...]
  rustpy lsp [global options]
  rustpy remote-server --root <absolute-directory> --admit-client
  rustpy completions <bash|zsh|fish|powershell>
  rustpy install-launcher [<dir-or-file>] [--force]
  rustpy diagnostics [--output <archive.tar.gz>]
  rustpy help [<command>]

Opening files and projects:
  <path>                A folder opens as a project, a file in the editor. When RustPy
                        already runs with the same configuration directory, the request
                        goes to that window.
  -l, --line <n>        Put the caret on line <n> of the next file (1-based).
  -c, --column <n>      ... and in column <n> (needs --line).
  -w, --wait            Return only when the files' tabs are closed (or the folder's
                        project): use 'rustpy --wait' as $EDITOR or git's core.editor.
  --new-window          Start another window instead of using the running one.
  --software-rendering  Draw with software OpenGL instead of the GPU (Linux: Mesa
                        llvmpipe); used automatically when the GPU context fails.

Global options:
  --headless            Refuse every command that opens or forwards to a GUI window.
  --config-dir <dir>    Settings, templates and schemes (default ~/.config/rustpy;
                        also RUSTPY_CONFIG_DIR). Each directory has its own instance.
  --system-dir <dir>    Local History and logs (default: the configuration directory;
                        also RUSTPY_SYSTEM_DIR).
  --log-dir <dir>       Where rustpy.log is written (default <system-dir>/log;
                        also RUSTPY_LOG_DIR).
  --disable-plugins     Start without user-installed extensions (TextMate bundles).
  --verbose             Debug logging.
  -h, --help            Show this help ('rustpy help <command>' for a command).
  -V, --version         Print the version.

Headless UI testing (see docs/configuration-directories.md):
  --open-file <file>  --goto <line>:<col>  --run  --welcome  --action <id>
  --tool <name>  --terminal <cmd>  --input-script <file>  --screenshot <png>

Exit status: 0 on success, 2 for invalid arguments; see 'rustpy help <command>'.
";

const OPEN: &str = "\
Usage: rustpy [options] [--line <n>] [--column <n>] [--wait] [<path> ...]

Opens folders as projects and files in the editor, with the caret at --line and
--column (both 1-based; they apply to the file that follows them). Without a path
RustPy reopens the last project (unless 'dontReopenProjects' is given).

When an IDE window already runs for the same configuration directory, the request
is sent to it (it opens the files and comes to the front) and this command returns
at once, or with --wait when the files' tabs are closed. A folder opens as File ›
Open does: in this window, a new window or after asking (Settings › Appearance &
Behavior › System Settings). --new-window always starts a separate window.

Examples:
  rustpy ~/projects/app
  rustpy --line 42 --column 7 src/app/models.py
  git config --global core.editor 'rustpy --wait'
";

const DIFF: &str = "\
Usage: rustpy diff <left> <right> [<base>]

Opens the diff viewer on two files, or on three files with a base version
(Left ↔ Base, Base ↔ Right and Left ↔ Right). Close the window or press Escape to
return. Exit status: 0, or 2 when a file cannot be read.

Example (git difftool):
  git config --global difftool.rustpy.cmd 'rustpy diff \"$LOCAL\" \"$REMOTE\"'
";

const MERGE: &str = "\
Usage: rustpy merge <left> <right> [<base>] <output>

Opens the three-way merge window: changes made on only one side are merged
automatically, conflicts are marked in the editable result, and Accept Left,
Accept Right and the per-conflict buttons resolve them. Apply writes the result
to <output>.

Exit status: 0 when the result was applied, 1 when the merge was cancelled (the
output file is left as it was), 2 when a file cannot be read.

Example (git mergetool):
  git config --global mergetool.rustpy.cmd 'rustpy merge \"$LOCAL\" \"$REMOTE\" \"$BASE\" \"$MERGED\"'
  git config --global mergetool.rustpy.trustExitCode true
";

const FORMAT: &str = "\
Usage: rustpy format [-h] [-s|-settings <file>] [-r|-R] [-m|-mask <masks>]
                     [-charset <name>] [-d|-dry] [-allowDefaults] <path> ...

Reformats Python files with RustPy's formatter (Code › Reformat Code) without
opening a window.

  -h, -help          Show this help.
  -s, -settings      Code style to use instead of the project's: a RustPy settings
                     file (settings.json or an exported settings file), an XML
                     code style scheme (.xml) or a pyproject.toml.
  -r, -R             Process directories recursively (skipping excluded folders,
                     virtual environments, caches and .gitignore'd files).
  -m, -mask          Comma-separated file name masks, e.g. '*.py,*.pyi,bin/*'.
                     Default: Python files (*.py, *.pyi).
  -charset           Read and write files in this encoding (UTF-8, UTF-16LE,
                     UTF-16BE, ISO-8859-1). Default: detected per file.
  -d, -dry           Only report: nothing is written.
  -allowDefaults     Format files outside any project with the default code style
                     (otherwise they are skipped).

The code style is the IDE's (Settings › Editor › Code Style) with .editorconfig
applied per file; when Settings › Code Style selects Black or Ruff as the formatter,
the [tool.black] or [tool.ruff] table of the nearest pyproject.toml is used instead.
A file belongs to a project when a folder above it has .rustpy, .idea, .git,
pyproject.toml, setup.py or setup.cfg.

Exit status: 0 on success; 1 when -dry finds a file that needs reformatting;
2 for invalid arguments, missing paths or files that cannot be formatted.
";

const INSPECT: &str = "\
Usage: rustpy inspect <project> <profile> <output-dir> [-d <dir>] [-changes]
                      [-format xml|json|plain|sarif] [-v0|-v1|-v2]

Runs the code inspections over a project without opening a window.

  <project>          The project folder.
  <profile>          'Project Default' (the IDE's Settings › Editor › Inspections),
                     'Default' (the built-in defaults), a profile name found in
                     <project>/.idea/inspectionProfiles, or a profile file: a RustPy
                     settings JSON file or an XML inspection profile (.xml).
                     -profileName <name> / -profilePath <file> also work.
  <output-dir>       Where the results are written (created if needed).
  -d <dir>           Inspect only this folder of the project.
  -changes           Inspect only files with uncommitted Git changes.
  -format <format>   xml (default; one <Inspection>.xml per inspection plus
                     .descriptions.xml), json (the same as .json),
                     plain (printed, and saved as inspections.txt) or sarif (one
                     SARIF 2.1.0 log, results.sarif).
  -v0, -v1, -v2      Quiet, summary (default), or every file and problem.

Exit status: 0 when no problem has the Error severity, 1 when some do, 2 when the
inspection could not run (bad arguments, unknown profile, unwritable output).
";

const TEST: &str = "\
Usage: rustpy test [--headless] [options] [<target> ...] [-- <framework-argument> ...]

Runs the IDE test engine without a window. Targets use paths or pytest-style node
ids; absent targets run the project suite. Framework arguments after -- are passed
unchanged as argv, without a shell.

  --project <dir>       Project root (default: current directory).
  --framework <name>    auto, pytest, unittest, doctest, nose, nose2, trial, behave,
                        tox or lettuce (default: configured runner, then detection).
  --interpreter <path>  Override module/project SDK, local .venv/venv and PATH Python.
  --cwd <dir>           Working directory relative to the project (where supported).
  --python-option <arg> Repeat for interpreter options such as -X and dev.
  --env <NAME=value>    Repeat for environment overrides; inherited by default.
  --no-inherit-env      Start the test process with an empty environment.
  --timeout <seconds>   Positive deadline, including automatic framework detection.
  --json               Write one JSON result object; console output goes to stderr.
  --junit <file>        Also write a JUnit XML report (relative to launcher cwd).

Project settings supply framework, pytest arguments/path and unittest discovery
preferences. Explicit command options win. SIGINT, SIGTERM/SIGHUP (Unix) and
Ctrl-C/Break (Windows) stop the owned test process tree and drain final results.

Exit status: 0 passed, 1 failures, 2 setup/protocol/report error, 5 no tests,
124 timeout, 130 cancellation. A runner's nonzero exit never counts as a pass.

Example:
  rustpy --headless test --project . --framework pytest --junit tests.xml -- -q
";

const COMPLETIONS: &str = "\
Usage: rustpy completions <bash|zsh|fish|powershell>

Prints a shell completion script. For example:
  rustpy completions bash > ~/.local/share/bash-completion/completions/rustpy
  rustpy completions zsh > \"${fpath[1]}/_rustpy\"
  rustpy completions fish > ~/.config/fish/completions/rustpy.fish
  rustpy completions powershell >> $PROFILE
";

const INSTALL_LAUNCHER: &str = "\
Usage: rustpy install-launcher [<dir-or-file>] [--force]

Writes a launcher script that starts this RustPy binary (Tools › Create
Command-line Launcher… does the same). The default is ~/.local/bin/rustpy on Linux
and macOS and %LOCALAPPDATA%\\Programs\\RustPy\\bin\\rustpy.cmd on Windows. An
existing directory, or a path ending in a separator, receives a 'rustpy' script;
any other path is the script itself. A file that is not a RustPy launcher is only
replaced with --force.
";

const HTTP: &str = "\
Usage: rustpy http <file.http> [--env name] [--request name-or-index]
                   [--var key=value] [--timeout ms] [--json]

Runs captured HTTP requests, environments, authentication helpers, JavaScript
handlers and protocol adapters.

Exit status: 0 when all requests and assertions succeed, 1 on request failures,
2 on invalid input.
";

const REMOTE_SERVER: &str = "\
Usage: rustpy remote-server --root DIRECTORY --admit-client

Runs the bounded Gateway protocol over stdio for one explicitly admitted client.
Requires matching client/server versions. Does not execute project startup hooks
or probe interpreters. Use an authenticated SSH/container/WSL transport.
";

const LSP: &str = "\
Usage: rustpy lsp [global options]

Runs the language server over stdio. Logs never go to stdout.
Global options are accepted before or after lsp.
";

/// The usage text for `command` (`None`: the overview).
const DIAGNOSTICS: &str = "\
Usage: rustpy diagnostics [--output <archive.tar.gz>] [global options]

Explicitly collects a local archive with bounded log tails, recent crash reports
and version/platform metadata. Nothing is uploaded. Settings, environment
variables, credential databases and project files are excluded. Logs can contain
paths or server details: inspect the archive before sharing it.

Without --output, saves a unique archive in <log-dir>/diagnostics/. Existing
destinations are never overwritten. Collection notices appear on stderr and in
report.json inside the archive. Output contains the archive path.
Exit status: 0 on success, 1 when the archive cannot be written, 2 for bad options.
";

pub fn usage(command: Option<&str>) -> String {
    let text = match command {
        None => MAIN,
        Some("open") => OPEN,
        Some("diff") => DIFF,
        Some("merge") => MERGE,
        Some("http") => HTTP,
        Some("format") => FORMAT,
        Some("inspect") => INSPECT,
        Some("test") => TEST,
        Some("remote-server") => REMOTE_SERVER,
        Some("lsp") => LSP,
        Some("completions") => COMPLETIONS,
        Some("install-launcher") => INSTALL_LAUNCHER,
        Some("diagnostics") => DIAGNOSTICS,
        Some(_) => MAIN,
    };
    text.replace("{version}", VERSION)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_command_has_help() {
        for c in crate::args::COMMANDS.iter().filter(|c| **c != "help") {
            let text = usage(Some(c));
            assert_ne!(text, usage(None), "{c}");
            assert!(text.contains(&format!("rustpy {c}")), "{c}: {text}");
        }
        assert!(usage(None).contains(VERSION));
    }

    #[test]
    fn help_lines_fit_a_terminal() {
        for c in [None, Some("open")]
            .into_iter()
            .chain(crate::args::COMMANDS.iter().copied().map(Some))
        {
            for line in usage(c).lines() {
                assert!(line.chars().count() <= 100, "{c:?}: {line}");
            }
        }
    }
}

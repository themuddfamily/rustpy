//! Command-line parsing for the `rustpy` launcher (roadmap §8 "Command-line
//! launcher"): `rustpy [--line N] [--column M] [--wait] path…`,
//! `rustpy diff`, `rustpy merge`, `rustpy format` and `rustpy inspect`.
//!
//! The first argument that is not a global option picks the command: `diff`,
//! `merge`, `format`, `inspect`, `lsp`, `completions`, `install-launcher` or `help`.
//! Anything else opens files and folders in the IDE, so a folder that is
//! literally called `format` is opened with `rustpy ./format`.

use std::fmt;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Options accepted before (and between) the arguments of every command.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GlobalOptions {
    /// Refuse every command that would create or forward to a GUI window.
    pub headless: bool,
    /// `--config-dir`: settings, templates, color schemes (`RUSTPY_CONFIG_DIR`).
    pub config_dir: Option<PathBuf>,
    /// `--system-dir`: Local History and the log (`RUSTPY_SYSTEM_DIR`).
    pub system_dir: Option<PathBuf>,
    /// `--log-dir`: where `rustpy.log` goes (`RUSTPY_LOG_DIR`).
    pub log_dir: Option<PathBuf>,
    /// `--disable-plugins` (also accepted as `disableNonBundledPlugins`).
    pub disable_plugins: bool,
    /// `--verbose`: debug logging.
    pub verbose: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cli {
    pub global: GlobalOptions,
    pub command: Command,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    Test(TestArgs),
    Http(HttpArgs),
    /// Language server over stdio.
    Lsp,
    RemoteServer {
        root: PathBuf,
        admit_client: bool,
    },
    /// `rustpy [--line N] [--column M] [--wait] [path …]`: the IDE.
    Open(OpenArgs),
    /// `rustpy diff <left> <right> [<base>]`.
    Diff(DiffArgs),
    /// `rustpy merge <left> <right> [<base>] <output>`.
    Merge(MergeArgs),
    /// `rustpy format …` (headless).
    Format(FormatArgs),
    /// `rustpy inspect …` (headless).
    Inspect(InspectArgs),
    /// `rustpy completions <shell>`.
    Completions(Shell),
    /// `rustpy install-launcher [path]`.
    InstallLauncher(LauncherArgs),
    /// `rustpy diagnostics [--output FILE]`: explicitly collect local logs.
    Diagnostics(Option<PathBuf>),
    /// `rustpy --help`, `rustpy help [command]`, `rustpy <command> -h`.
    Help(Option<String>),
    /// `rustpy --version`.
    Version,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TestArgs {
    /// Internal supervised framework detector; never opens a window.
    pub(crate) probe_framework: bool,
    pub project: PathBuf,
    pub framework: String,
    pub interpreter: Option<PathBuf>,
    pub targets: Vec<String>,
    pub extra: Vec<String>,
    pub env: Vec<(String, String)>,
    pub inherit_environment: bool,
    pub cwd: Option<PathBuf>,
    pub interpreter_options: Vec<String>,
    pub timeout_seconds: Option<u64>,
    pub json: bool,
    pub junit: Option<PathBuf>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HttpArgs {
    pub file: PathBuf,
    pub environment: String,
    pub request: Option<String>,
    pub timeout_ms: u64,
    pub variables: std::collections::HashMap<String, String>,
    pub json: bool,
}
/// A file or folder to open, with the caret position for files (1-based).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenTarget {
    pub path: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub column: Option<u32>,
}

impl OpenTarget {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            line: None,
            column: None,
        }
    }
}

/// Arguments of the IDE itself.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct OpenArgs {
    pub targets: Vec<OpenTarget>,
    /// `--wait`: block until the files' tabs (or the folder's project) are closed.
    pub wait: bool,
    /// `--new-window`: create an independent project window. On macOS the
    /// authenticated native project host owns its same-process viewport.
    pub new_window: bool,
    /// `dontReopenProjects`: start on the Welcome screen when no path is given.
    pub dont_reopen_projects: bool,
    /// Internal checkpoint/runtime-generation marker. Never initiates fan-out.
    pub restore_child: Option<String>,
    /// `--software-rendering`: draw with the CPU OpenGL implementation instead of
    /// the GPU driver (also Settings › Appearance › Use software rendering).
    pub software_rendering: bool,
    // ----- headless UI testing (scripts/smoke.sh) -----------------------------
    pub screenshot: Option<PathBuf>,
    pub open_file: Option<PathBuf>,
    pub run: bool,
    pub welcome: bool,
    pub terminal: Option<String>,
    pub tool: Option<String>,
    /// `--goto line:col` (1-based) in the active editor.
    pub goto: Option<(usize, usize)>,
    pub actions: Vec<String>,
    pub input_script: Option<PathBuf>,
    /// Problems that did not stop the launch (unknown options), for stderr.
    pub warnings: Vec<String>,
}

impl OpenArgs {
    /// Smoke-test runs (`--screenshot`, `--input-script`) never forward to a
    /// running IDE nor accept forwarded requests: parallel test runs share the
    /// configuration directory set by `scripts/smoke.sh`.
    pub fn is_smoke_test(&self) -> bool {
        self.screenshot.is_some() || self.input_script.is_some()
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DiffArgs {
    pub left: PathBuf,
    pub right: PathBuf,
    pub base: Option<PathBuf>,
    /// Headless UI testing: capture the window into this PNG, then close it.
    pub screenshot: Option<PathBuf>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MergeArgs {
    pub left: PathBuf,
    pub right: PathBuf,
    pub base: Option<PathBuf>,
    pub output: PathBuf,
    /// Headless UI testing: capture the window into this PNG, then close it
    /// (as Cancel).
    pub screenshot: Option<PathBuf>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FormatArgs {
    /// `-s|-settings <file>`: the code style to use instead of the project's.
    pub settings: Option<PathBuf>,
    /// `-r|-R`: descend into subdirectories.
    pub recursive: bool,
    /// `-m|-mask <masks>`: comma-separated file name masks (default: Python files).
    pub masks: Vec<String>,
    /// `-charset <name>`: read and write files in this encoding.
    pub charset: Option<String>,
    /// `-d|-dry`: report only; exit code 1 when a file needs reformatting.
    pub dry: bool,
    /// `-allowDefaults`: format files outside any project with the default style.
    pub allow_defaults: bool,
    pub paths: Vec<PathBuf>,
}

/// Output format of `rustpy inspect`.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum ReportFormat {
    #[default]
    Xml,
    Json,
    Plain,
    Sarif,
}

impl ReportFormat {
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "xml" => Some(Self::Xml),
            "json" => Some(Self::Json),
            "plain" | "text" | "txt" => Some(Self::Plain),
            "sarif" => Some(Self::Sarif),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InspectArgs {
    pub project: PathBuf,
    /// A profile name (`Project Default`) or a profile file.
    pub profile: String,
    pub output: PathBuf,
    /// `-d <dir>`: inspect only this directory of the project.
    pub dir: Option<PathBuf>,
    /// `-changes`: inspect only files with uncommitted changes (Git).
    pub changes: bool,
    pub format: ReportFormat,
    /// `-v0` quiet, `-v1` (default) summary, `-v2` every file and problem.
    pub verbosity: u8,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Shell {
    Bash,
    Zsh,
    Fish,
    PowerShell,
}

impl Shell {
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "bash" => Some(Self::Bash),
            "zsh" => Some(Self::Zsh),
            "fish" => Some(Self::Fish),
            "powershell" | "pwsh" => Some(Self::PowerShell),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LauncherArgs {
    /// A directory (the script is `rustpy` in it) or the script's path.
    pub path: Option<PathBuf>,
    /// Replace an existing file that is not a RustPy launcher script.
    pub force: bool,
}

/// A command line that cannot be run; printed with a hint to `--help`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArgError {
    pub message: String,
    /// The command whose usage applies (`format`, …), or `None` for the IDE.
    pub command: Option<&'static str>,
}

impl ArgError {
    fn new(command: Option<&'static str>, message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            command,
        }
    }
}

impl fmt::Display for ArgError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ArgError {}

/// The subcommand names (shell completion uses the same list).
pub const COMMANDS: &[&str] = &[
    "test",
    "http",
    "diff",
    "merge",
    "format",
    "inspect",
    "lsp",
    "remote-server",
    "completions",
    "install-launcher",
    "diagnostics",
    "help",
];

#[cfg(test)]
mod diagnostics_argument_tests {
    use super::*;

    #[test]
    fn diagnostics_accepts_explicit_outputs_and_global_options_without_becoming_a_window() {
        assert_eq!(
            parse(["diagnostics"]).unwrap().command,
            Command::Diagnostics(None)
        );
        let cli = parse([
            "diagnostics",
            "--output=report.tar.gz",
            "--log-dir",
            "logs",
            "--verbose",
        ])
        .unwrap();
        assert_eq!(
            cli.command,
            Command::Diagnostics(Some("report.tar.gz".into()))
        );
        assert_eq!(cli.global.log_dir, Some("logs".into()));
        assert!(cli.global.verbose);
        assert_eq!(
            parse(["diagnostics", "-h"]).unwrap().command,
            Command::Help(Some("diagnostics".into()))
        );
        assert_eq!(
            parse(["help", "diagnostics"]).unwrap().command,
            Command::Help(Some("diagnostics".into()))
        );
        assert!(matches!(
            parse(["./diagnostics"]).unwrap().command,
            Command::Open(_)
        ));
        for args in [
            vec!["diagnostics", "--output"],
            vec!["diagnostics", "--output="],
            vec!["diagnostics", "--output", "a", "--output", "b"],
            vec!["diagnostics", "extra"],
        ] {
            assert_eq!(parse(args).unwrap_err().command, Some("diagnostics"));
        }
    }
}

/// A cursor over the arguments. `--name=value` is split into the option and a
/// pending value.
struct Args {
    items: Vec<String>,
    pos: usize,
    /// The value of the last `--name=value` option, not yet taken.
    inline: Option<String>,
}

impl Args {
    fn new(items: Vec<String>) -> Self {
        Self {
            items,
            pos: 0,
            inline: None,
        }
    }

    fn peek(&self) -> Option<&str> {
        self.items.get(self.pos).map(String::as_str)
    }

    /// The next argument; a long option written `--name=value` comes back as
    /// `--name` with `value` pending for [`Args::value`].
    fn next(&mut self) -> Option<String> {
        let a = self.items.get(self.pos)?.clone();
        self.pos += 1;
        self.inline = None;
        if a.starts_with("--") && a.len() > 2 {
            if let Some((name, value)) = a.split_once('=') {
                self.inline = Some(value.to_string());
                return Some(name.to_string());
            }
        }
        Some(a)
    }

    /// The value of option `name`.
    fn value(&mut self, name: &str, command: Option<&'static str>) -> Result<String, ArgError> {
        if let Some(v) = self.inline.take() {
            return Ok(v);
        }
        match self.items.get(self.pos) {
            Some(v) => {
                self.pos += 1;
                Ok(v.clone())
            }
            None => Err(ArgError::new(
                command,
                format!("option '{name}' needs a value"),
            )),
        }
    }

    /// An option that takes no value was written `--name=value`.
    fn reject_inline(&mut self, name: &str, command: Option<&'static str>) -> Result<(), ArgError> {
        match self.inline.take() {
            Some(_) => Err(ArgError::new(
                command,
                format!("option '{name}' takes no value"),
            )),
            None => Ok(()),
        }
    }
}

/// Handles a global option; returns false when `arg` is not one.
fn global_option(
    arg: &str,
    it: &mut Args,
    global: &mut GlobalOptions,
    command: Option<&'static str>,
) -> Result<bool, ArgError> {
    match arg {
        "--headless" => {
            it.reject_inline(arg, command)?;
            global.headless = true;
        }
        "--config-dir" => {
            global.config_dir = Some(nonempty_path(arg, it.value(arg, command)?, command)?)
        }
        "--system-dir" => {
            global.system_dir = Some(nonempty_path(arg, it.value(arg, command)?, command)?)
        }
        "--log-dir" => global.log_dir = Some(nonempty_path(arg, it.value(arg, command)?, command)?),
        "--disable-plugins" | "disableNonBundledPlugins" => {
            it.reject_inline(arg, command)?;
            global.disable_plugins = true;
        }
        "--verbose" => {
            it.reject_inline(arg, command)?;
            global.verbose = true;
        }
        _ => return Ok(false),
    }
    Ok(true)
}

fn nonempty_path(
    name: &str,
    value: String,
    command: Option<&'static str>,
) -> Result<PathBuf, ArgError> {
    if value.is_empty() {
        return Err(ArgError::new(
            command,
            format!("option '{name}' needs a directory"),
        ));
    }
    Ok(PathBuf::from(value))
}

fn number(name: &str, value: &str, command: Option<&'static str>) -> Result<u32, ArgError> {
    match value.trim().parse::<u32>() {
        Ok(n) if n >= 1 => Ok(n),
        _ => Err(ArgError::new(
            command,
            format!("option '{name}' needs a positive number, not '{value}'"),
        )),
    }
}

/// Parses the arguments after the program name.
pub fn parse<I, S>(args: I) -> Result<Cli, ArgError>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    let mut it = Args::new(args.into_iter().map(Into::into).collect());
    let mut global = GlobalOptions::default();
    // Global options before the command.
    while let Some(a) = it.peek() {
        if !a.starts_with("--") && a != "disableNonBundledPlugins" {
            break;
        }
        let save = it.pos;
        let a = it.next().unwrap_or_default();
        if !global_option(&a, &mut it, &mut global, None)? {
            it.pos = save;
            it.inline = None;
            break;
        }
    }
    let command = match it.peek() {
        Some("test") => {
            it.next();
            parse_test(&mut it, &mut global)?
        }
        Some("remote-server") => {
            it.next();
            let mut root = None;
            let mut admitted = false;
            let mut help = false;
            while let Some(arg) = it.next() {
                if global_option(&arg, &mut it, &mut global, Some("remote-server"))? {
                    continue;
                }
                match arg.as_str() {
                    "--root" => {
                        if root.is_some() {
                            return Err(ArgError::new(
                                Some("remote-server"),
                                "--root specified twice",
                            ));
                        }
                        root = Some(PathBuf::from(it.value("--root", Some("remote-server"))?));
                    }
                    "--admit-client" => {
                        it.reject_inline(&arg, Some("remote-server"))?;
                        admitted = true;
                    }
                    "--help" | "-h" => help = true,
                    _ => {
                        return Err(ArgError::new(
                            Some("remote-server"),
                            format!("unexpected argument '{arg}'"),
                        ))
                    }
                }
            }
            if help {
                Command::Help(Some("remote-server".into()))
            } else {
                Command::RemoteServer {
                    root: root.ok_or_else(|| {
                        ArgError::new(Some("remote-server"), "--root is required")
                    })?,
                    admit_client: admitted,
                }
            }
        }
        Some("lsp") => {
            it.next();
            let mut command = Command::Lsp;
            while let Some(arg) = it.next() {
                if global_option(&arg, &mut it, &mut global, Some("lsp"))? {
                    continue;
                }
                match arg.as_str() {
                    "-h" | "--help" => {
                        it.reject_inline(&arg, Some("lsp"))?;
                        command = Command::Help(Some("lsp".into()));
                    }
                    _ => {
                        return Err(ArgError::new(
                            Some("lsp"),
                            format!("unexpected argument '{arg}'"),
                        ))
                    }
                }
            }
            command
        }
        Some("http") => {
            it.next();
            parse_http(&mut it, &mut global)?
        }
        Some("diff") => {
            it.next();
            parse_diff(&mut it, &mut global)?
        }
        Some("merge") => {
            it.next();
            parse_merge(&mut it, &mut global)?
        }
        Some("format") => {
            it.next();
            parse_format(&mut it, &mut global)?
        }
        Some("inspect") => {
            it.next();
            parse_inspect(&mut it, &mut global)?
        }
        Some("completions") => {
            it.next();
            parse_completions(&mut it, &mut global)?
        }
        Some("install-launcher") => {
            it.next();
            parse_install_launcher(&mut it, &mut global)?
        }
        Some("diagnostics") => {
            it.next();
            let mut output = None;
            let mut help = false;
            while let Some(arg) = it.next() {
                if global_option(&arg, &mut it, &mut global, Some("diagnostics"))? {
                    continue;
                }
                match arg.as_str() {
                    "--output" => {
                        if output.is_some() {
                            return Err(ArgError::new(
                                Some("diagnostics"),
                                "--output may only be supplied once",
                            ));
                        }
                        let value = it.value(&arg, Some("diagnostics"))?;
                        if value.is_empty() {
                            return Err(ArgError::new(
                                Some("diagnostics"),
                                "--output needs a file",
                            ));
                        }
                        output = Some(value.into());
                    }
                    "-h" | "--help" => {
                        it.reject_inline(&arg, Some("diagnostics"))?;
                        help = true;
                    }
                    _ => {
                        return Err(ArgError::new(
                            Some("diagnostics"),
                            format!("unexpected argument '{arg}'"),
                        ))
                    }
                }
            }
            if help {
                Command::Help(Some("diagnostics".into()))
            } else {
                Command::Diagnostics(output)
            }
        }
        Some("help") => {
            it.next();
            let topic = it.next();
            match topic {
                Some(t) if !COMMANDS.contains(&t.as_str()) && t != "open" => {
                    return Err(ArgError::new(
                        None,
                        format!(
                            "no help for '{t}': the commands are {}",
                            COMMANDS.join(", ")
                        ),
                    ))
                }
                _ => Command::Help(topic),
            }
        }
        _ => parse_open(&mut it, &mut global)?,
    };
    if global.headless
        && matches!(
            command,
            Command::Open(_) | Command::Diff(_) | Command::Merge(_)
        )
    {
        return Err(ArgError::new(None, "--headless requires a non-window command (format, inspect, test, http, lsp, help, completions, diagnostics or install-launcher)"));
    }
    Ok(Cli { global, command })
}

fn parse_test(it: &mut Args, global: &mut GlobalOptions) -> Result<Command, ArgError> {
    let mut args = TestArgs {
        probe_framework: false,
        project: PathBuf::from("."),
        framework: "auto".into(),
        interpreter: None,
        targets: Vec::new(),
        extra: Vec::new(),
        env: Vec::new(),
        inherit_environment: true,
        cwd: None,
        interpreter_options: Vec::new(),
        timeout_seconds: None,
        json: false,
        junit: None,
    };
    while let Some(option) = it.next() {
        if option == "--" {
            args.extra.extend(it.items[it.pos..].iter().cloned());
            it.pos = it.items.len();
            break;
        }
        if global_option(&option, it, global, Some("test"))? {
            continue;
        }
        match option.as_str() {
            "--internal-framework-probe" => {
                it.reject_inline(&option, Some("test"))?;
                args.probe_framework = true;
            }
            "-h" | "--help" => return Ok(Command::Help(Some("test".into()))),
            "--project" => {
                args.project =
                    nonempty_path(&option, it.value(&option, Some("test"))?, Some("test"))?
            }
            "--interpreter" => {
                args.interpreter = Some(nonempty_path(
                    &option,
                    it.value(&option, Some("test"))?,
                    Some("test"),
                )?)
            }
            "--framework" => {
                args.framework = it.value(&option, Some("test"))?;
                if args.framework != "auto"
                    && rustpy_test::Framework::from_key(&args.framework).is_none()
                {
                    return Err(ArgError::new(Some("test"), "unknown test framework"));
                }
            }
            "--cwd" => {
                args.cwd = Some(nonempty_path(
                    &option,
                    it.value(&option, Some("test"))?,
                    Some("test"),
                )?)
            }
            "--python-option" => args
                .interpreter_options
                .push(it.value(&option, Some("test"))?),
            "--timeout" => {
                let seconds = it
                    .value(&option, Some("test"))?
                    .parse::<u64>()
                    .ok()
                    .filter(|n| *n > 0)
                    .ok_or_else(|| {
                        ArgError::new(Some("test"), "--timeout requires positive seconds")
                    })?;
                args.timeout_seconds = Some(seconds);
            }
            "--env" => {
                let value = it.value(&option, Some("test"))?;
                let (key, value) = value
                    .split_once('=')
                    .filter(|(key, value)| {
                        !key.is_empty() && !key.contains('\0') && !value.contains('\0')
                    })
                    .ok_or_else(|| ArgError::new(Some("test"), "--env requires NAME=value"))?;
                args.env.push((key.into(), value.into()));
            }
            "--no-inherit-env" => {
                it.reject_inline(&option, Some("test"))?;
                args.inherit_environment = false;
            }
            "--json" => {
                it.reject_inline(&option, Some("test"))?;
                args.json = true;
            }
            "--junit" => {
                args.junit = Some(nonempty_path(
                    &option,
                    it.value(&option, Some("test"))?,
                    Some("test"),
                )?)
            }
            value if value.starts_with('-') => {
                return Err(ArgError::new(
                    Some("test"),
                    format!("unknown option '{value}'; pass framework options after --"),
                ))
            }
            _ => args.targets.push(option),
        }
    }
    Ok(Command::Test(args))
}

#[cfg(test)]
mod ci_tests {
    use super::*;
    #[test]
    fn headless_never_falls_back_to_windows_even_for_typos() {
        for arguments in [
            vec!["--headless"],
            vec!["--headless", "tset"],
            vec!["project", "--headless"],
            vec!["--headless", "diff", "a", "b"],
            vec!["merge", "a", "b", "out", "--headless"],
        ] {
            assert!(parse(arguments).is_err());
        }
        for arguments in [
            vec!["--headless", "test", "--help"],
            vec!["--headless", "lsp"],
            vec!["format", "--headless", "a.py"],
            vec!["--headless", "inspect", ".", "Default", "out"],
        ] {
            assert!(!crate::opens_window(&parse(arguments).unwrap().command));
        }
    }
    #[test]
    fn ci_keeps_exact_framework_argv_and_environment_values() {
        let cli = parse([
            "--headless",
            "test",
            "--project=app",
            "--framework",
            "pytest",
            "--interpreter",
            "my python",
            "--env",
            "TOKEN=x=y",
            "--no-inherit-env",
            "--timeout=30",
            "--json",
            "--junit=out.xml",
            "tests/test_a.py::test_a",
            "--",
            "--capture=tee-sys",
            "-k",
            "space or quotes",
            "--headless=false",
        ])
        .unwrap();
        let Command::Test(args) = cli.command else {
            panic!("test command")
        };
        assert!(cli.global.headless);
        assert_eq!(
            args.extra,
            [
                "--capture=tee-sys",
                "-k",
                "space or quotes",
                "--headless=false"
            ]
        );
        assert_eq!(args.env, [("TOKEN".into(), "x=y".into())]);
        assert_eq!(args.timeout_seconds, Some(30));
        assert!(!args.inherit_environment);
        assert_eq!(args.targets, ["tests/test_a.py::test_a"]);
    }
    #[test]
    fn invalid_test_options_do_not_turn_into_open_requests() {
        for arguments in [
            vec!["test", "--framework=unknown"],
            vec!["test", "--timeout=0"],
            vec!["test", "--env=EMPTY"],
            vec!["test", "--json=yes"],
            vec!["test", "--interpreter="],
            vec!["test", "--unknown"],
            vec!["--headless=true", "test"],
        ] {
            assert!(parse(arguments).is_err());
        }
    }
}

#[cfg(test)]
#[test]
fn lsp_accepts_global_options_on_either_side() {
    let cli = parse(["--config-dir", "cfg", "lsp", "--log-dir=logs", "--verbose"]).unwrap();
    assert_eq!(cli.command, Command::Lsp);
    assert_eq!(cli.global.config_dir, Some(PathBuf::from("cfg")));
    assert_eq!(cli.global.log_dir, Some(PathBuf::from("logs")));
    assert!(cli.global.verbose);
    assert_eq!(
        parse(["lsp", "--help"]).unwrap().command,
        Command::Help(Some("lsp".into()))
    );
    assert!(parse(["lsp", "--bogus"]).is_err());
    assert!(parse(["lsp", "--config-dir"]).is_err());
    assert!(matches!(
        parse(["./lsp"]).unwrap().command,
        Command::Open(_)
    ));
}

fn parse_open(it: &mut Args, global: &mut GlobalOptions) -> Result<Command, ArgError> {
    let mut o = OpenArgs::default();
    let mut line: Option<u32> = None;
    let mut column: Option<u32> = None;
    let mut only_paths = false;
    while let Some(a) = it.next() {
        if only_paths || !a.starts_with('-') || a == "-" {
            if !only_paths {
                // Legacy launcher arguments without dashes.
                match a.as_str() {
                    "nosplash" => continue,
                    "dontReopenProjects" => {
                        o.dont_reopen_projects = true;
                        continue;
                    }
                    "disableNonBundledPlugins" => {
                        global.disable_plugins = true;
                        continue;
                    }
                    _ => {}
                }
            }
            if column.is_some() && line.is_none() {
                o.warnings
                    .push(format!("--column is ignored without --line for {a}"));
                column = None;
            }
            o.targets.push(OpenTarget {
                path: PathBuf::from(&a),
                line: line.take(),
                column: column.take(),
            });
            continue;
        }
        match a.as_str() {
            "--" => only_paths = true,
            "-l" | "--line" => line = Some(number(&a, &it.value(&a, None)?, None)?),
            "-c" | "--column" => column = Some(number(&a, &it.value(&a, None)?, None)?),
            "-w" | "--wait" => {
                it.reject_inline(&a, None)?;
                o.wait = true;
            }
            "--restore-child" => {
                let marker = it.value(&a, None)?;
                if marker.len() > 1024 {
                    return Err(ArgError::new(None, "restore child marker is too large"));
                }
                if o.restore_child.is_some() {
                    return Err(ArgError::new(None, "restore child marker is repeated"));
                }
                o.restore_child = Some(marker);
            }
            "--new-window" => {
                it.reject_inline(&a, None)?;
                o.new_window = true;
            }
            "--software-rendering" => {
                it.reject_inline(&a, None)?;
                o.software_rendering = true;
            }
            "-h" | "--help" | "-help" | "-?" => return Ok(Command::Help(None)),
            "-V" | "--version" | "-version" => return Ok(Command::Version),
            "--screenshot" => o.screenshot = Some(PathBuf::from(it.value(&a, None)?)),
            "--open-file" => o.open_file = Some(PathBuf::from(it.value(&a, None)?)),
            "--run" => o.run = true,
            "--welcome" => o.welcome = true,
            "--terminal" => o.terminal = Some(it.value(&a, None)?),
            "--tool" => o.tool = Some(it.value(&a, None)?),
            "--input-script" => o.input_script = Some(PathBuf::from(it.value(&a, None)?)),
            "--goto" => {
                let v = it.value(&a, None)?;
                let mut parts = v.split(':');
                let line = parts
                    .next()
                    .and_then(|l| l.trim().parse().ok())
                    .unwrap_or(1);
                let col = parts
                    .next()
                    .and_then(|c| c.trim().parse().ok())
                    .unwrap_or(1);
                o.goto = Some((line, col));
            }
            "--action" => o.actions.push(it.value(&a, None)?),
            // macOS passes a process serial number to apps started from Finder.
            _ if a.starts_with("-psn_") => {}
            _ => {
                if !global_option(&a, it, global, None)? {
                    o.warnings
                        .push(format!("unknown option '{a}' ignored (see rustpy --help)"));
                }
            }
        }
    }
    if line.is_some() || column.is_some() {
        return Err(ArgError::new(
            None,
            "--line and --column must be followed by the file they apply to",
        ));
    }
    Ok(Command::Open(o))
}

/// Collects the positional arguments of a command, handling `-h`, `--` and
/// global options; `option` handles the command's own options and returns
/// false for unknown ones.
fn positionals(
    it: &mut Args,
    global: &mut GlobalOptions,
    command: &'static str,
    mut option: impl FnMut(&str, &mut Args) -> Result<bool, ArgError>,
) -> Result<Result<Vec<String>, Command>, ArgError> {
    let mut out = Vec::new();
    let mut only_paths = false;
    while let Some(a) = it.next() {
        if only_paths || !a.starts_with('-') || a == "-" {
            out.push(a);
            continue;
        }
        match a.as_str() {
            "--" => only_paths = true,
            "-h" | "-help" | "--help" | "-?" => {
                return Ok(Err(Command::Help(Some(command.to_string()))))
            }
            _ => {
                if option(&a, it)? || global_option(&a, it, global, Some(command))? {
                    continue;
                }
                return Err(ArgError::new(
                    Some(command),
                    format!("unknown option '{a}'"),
                ));
            }
        }
    }
    Ok(Ok(out))
}

/// `--screenshot <png>` of the diff and merge windows (headless UI tests).
fn screenshot_option<'a>(
    command: &'static str,
    screenshot: &'a mut Option<PathBuf>,
) -> impl FnMut(&str, &mut Args) -> Result<bool, ArgError> + 'a {
    move |a, it| {
        if a == "--screenshot" {
            *screenshot = Some(PathBuf::from(it.value(a, Some(command))?));
            return Ok(true);
        }
        Ok(false)
    }
}

fn parse_diff(it: &mut Args, global: &mut GlobalOptions) -> Result<Command, ArgError> {
    let mut screenshot = None;
    let args = match positionals(
        it,
        global,
        "diff",
        screenshot_option("diff", &mut screenshot),
    )? {
        Ok(a) => a,
        Err(help) => return Ok(help),
    };
    match args.as_slice() {
        [l, r] => Ok(Command::Diff(DiffArgs {
            left: l.into(),
            right: r.into(),
            base: None,
            screenshot,
        })),
        [l, r, b] => Ok(Command::Diff(DiffArgs {
            left: l.into(),
            right: r.into(),
            base: Some(b.into()),
            screenshot,
        })),
        _ => Err(ArgError::new(
            Some("diff"),
            format!("diff needs two or three files, got {}", args.len()),
        )),
    }
}

fn parse_merge(it: &mut Args, global: &mut GlobalOptions) -> Result<Command, ArgError> {
    let mut screenshot = None;
    let args = match positionals(
        it,
        global,
        "merge",
        screenshot_option("merge", &mut screenshot),
    )? {
        Ok(a) => a,
        Err(help) => return Ok(help),
    };
    match args.as_slice() {
        [l, r, out] => Ok(Command::Merge(MergeArgs {
            left: l.into(),
            right: r.into(),
            base: None,
            output: out.into(),
            screenshot,
        })),
        [l, r, b, out] => Ok(Command::Merge(MergeArgs {
            left: l.into(),
            right: r.into(),
            base: Some(b.into()),
            output: out.into(),
            screenshot,
        })),
        _ => Err(ArgError::new(
            Some("merge"),
            format!(
                "merge needs <left> <right> [<base>] <output>, got {} file(s)",
                args.len()
            ),
        )),
    }
}

/// `-name`, `--name` and `--name=value` spellings of a single-dash style option.
fn option_name(a: &str) -> &str {
    a.trim_start_matches('-')
}

fn parse_format(it: &mut Args, global: &mut GlobalOptions) -> Result<Command, ArgError> {
    let mut f = FormatArgs::default();
    let cmd = Some("format");
    let args = match positionals(it, global, "format", |a, it| {
        match option_name(a) {
            "s" | "settings" => f.settings = Some(PathBuf::from(it.value(a, cmd)?)),
            "r" | "R" | "recursive" => f.recursive = true,
            "m" | "mask" => f.masks.extend(
                it.value(a, cmd)?
                    .split(',')
                    .map(str::trim)
                    .filter(|m| !m.is_empty())
                    .map(str::to_string),
            ),
            "charset" => f.charset = Some(it.value(a, cmd)?),
            "d" | "dry" | "dry-run" => f.dry = true,
            "allowDefaults" | "allow-defaults" => f.allow_defaults = true,
            _ => return Ok(false),
        }
        Ok(true)
    })? {
        Ok(a) => a,
        Err(help) => return Ok(help),
    };
    if args.is_empty() {
        return Err(ArgError::new(
            cmd,
            "format needs at least one file or directory",
        ));
    }
    f.paths = args.into_iter().map(PathBuf::from).collect();
    Ok(Command::Format(f))
}

fn parse_http(it: &mut Args, global: &mut GlobalOptions) -> Result<Command, ArgError> {
    let mut environment = String::new();
    let mut request = None;
    let mut timeout_ms = 30_000;
    let mut variables = std::collections::HashMap::new();
    let mut json = false;
    let args = match positionals(it, global, "http", |argument, it| {
        match option_name(argument) {
            "env" => environment = it.value(argument, Some("http"))?,
            "request" => request = Some(it.value(argument, Some("http"))?),
            "timeout" => {
                timeout_ms = it
                    .value(argument, Some("http"))?
                    .parse()
                    .map_err(|_| ArgError::new(Some("http"), "Timeout must be milliseconds"))?
            }
            "var" => {
                let text = it.value(argument, Some("http"))?;
                let (key, value) = text
                    .split_once('=')
                    .ok_or_else(|| ArgError::new(Some("http"), "--var requires name=value"))?;
                variables.insert(key.into(), value.into());
            }
            "json" => json = true,
            _ => return Ok(false),
        }
        Ok(true)
    })? {
        Ok(args) => args,
        Err(help) => return Ok(help),
    };
    if args.len() != 1 {
        return Err(ArgError::new(
            Some("http"),
            "http requires one .http or .rest file",
        ));
    }
    Ok(Command::Http(HttpArgs {
        file: args[0].clone().into(),
        environment,
        request,
        timeout_ms,
        variables,
        json,
    }))
}

fn parse_inspect(it: &mut Args, global: &mut GlobalOptions) -> Result<Command, ArgError> {
    let cmd = Some("inspect");
    let mut dir: Option<PathBuf> = None;
    let mut changes = false;
    let mut format = ReportFormat::Xml;
    let mut verbosity = 1u8;
    let mut profile_option: Option<String> = None;
    let args = match positionals(it, global, "inspect", |a, it| {
        match option_name(a) {
            "d" => dir = Some(PathBuf::from(it.value(a, cmd)?)),
            "changes" => changes = true,
            "format" => {
                let v = it.value(a, cmd)?;
                format = ReportFormat::parse(&v).ok_or_else(|| {
                    ArgError::new(
                        cmd,
                        format!("unknown report format '{v}': use xml, json, plain or sarif"),
                    )
                })?;
            }
            "v0" => verbosity = 0,
            "v1" => verbosity = 1,
            "v2" | "v3" => verbosity = 2,
            "profileName" | "profilePath" => profile_option = Some(it.value(a, cmd)?),
            _ => return Ok(false),
        }
        Ok(true)
    })? {
        Ok(a) => a,
        Err(help) => return Ok(help),
    };
    let (project, profile, output) = match (args.as_slice(), profile_option) {
        ([project, profile, output], None) => (project.clone(), profile.clone(), output.clone()),
        ([project, output], Some(profile)) => (project.clone(), profile, output.clone()),
        ([_, _, _], Some(_)) => return Err(ArgError::new(
            cmd,
            "give the profile either as an argument or with -profileName / -profilePath, not both",
        )),
        _ => {
            return Err(ArgError::new(
                cmd,
                "inspect needs <project> <profile> <output-dir>",
            ))
        }
    };
    Ok(Command::Inspect(InspectArgs {
        project: project.into(),
        profile,
        output: output.into(),
        dir,
        changes,
        format,
        verbosity,
    }))
}

fn parse_completions(it: &mut Args, global: &mut GlobalOptions) -> Result<Command, ArgError> {
    let args = match positionals(it, global, "completions", |_, _| Ok(false))? {
        Ok(a) => a,
        Err(help) => return Ok(help),
    };
    match args.as_slice() {
        [s] => Shell::parse(s).map(Command::Completions).ok_or_else(|| {
            ArgError::new(
                Some("completions"),
                format!("unknown shell '{s}': use bash, zsh, fish or powershell"),
            )
        }),
        _ => Err(ArgError::new(
            Some("completions"),
            "completions needs a shell: bash, zsh, fish or powershell",
        )),
    }
}

fn parse_install_launcher(it: &mut Args, global: &mut GlobalOptions) -> Result<Command, ArgError> {
    let mut force = false;
    let args = match positionals(it, global, "install-launcher", |a, _| {
        if matches!(option_name(a), "f" | "force") {
            force = true;
            return Ok(true);
        }
        Ok(false)
    })? {
        Ok(a) => a,
        Err(help) => return Ok(help),
    };
    match args.as_slice() {
        [] => Ok(Command::InstallLauncher(LauncherArgs { path: None, force })),
        [p] => Ok(Command::InstallLauncher(LauncherArgs {
            path: Some(p.into()),
            force,
        })),
        _ => Err(ArgError::new(
            Some("install-launcher"),
            "install-launcher takes at most one directory or file",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gateway_requires_captured_root_and_preserves_explicit_admission() {
        assert!(parse(["remote-server"]).is_err());
        assert!(
            matches!(p(&["remote-server","--root","/chosen root","--admit-client"]).command,Command::RemoteServer{root,admit_client:true} if root==std::path::Path::new("/chosen root"))
        );
        assert!(matches!(
            p(&["remote-server", "--root", "/chosen"]).command,
            Command::RemoteServer {
                admit_client: false,
                ..
            }
        ));
        assert!(!crate::opens_window(
            &p(&["remote-server", "--root", "/chosen"]).command
        ));
        assert!(parse(["remote-server", "--root", "/a", "--root", "/b"]).is_err());
    }

    fn p(args: &[&str]) -> Cli {
        parse(args.iter().copied()).unwrap_or_else(|e| panic!("{args:?}: {e}"))
    }

    fn open(args: &[&str]) -> OpenArgs {
        match p(args).command {
            Command::Open(o) => o,
            other => panic!("{args:?}: {other:?}"),
        }
    }

    fn err(args: &[&str]) -> ArgError {
        parse(args.iter().copied()).expect_err(&format!("{args:?} should fail"))
    }

    #[test]
    fn no_arguments_start_the_ide() {
        assert_eq!(p(&[]).command, Command::Open(OpenArgs::default()));
    }

    #[test]
    fn line_and_column_apply_to_the_next_file_only() {
        let o = open(&[
            "--line", "12", "--column", "4", "a.py", "b.py", "-l", "3", "c.py", "proj",
        ]);
        assert_eq!(
            o.targets,
            vec![
                OpenTarget {
                    path: "a.py".into(),
                    line: Some(12),
                    column: Some(4)
                },
                OpenTarget::new("b.py"),
                OpenTarget {
                    path: "c.py".into(),
                    line: Some(3),
                    column: None
                },
                OpenTarget::new("proj"),
            ]
        );
        let o = open(&["--line=7", "--column=2", "x.py"]);
        assert_eq!(
            o.targets[0],
            OpenTarget {
                path: "x.py".into(),
                line: Some(7),
                column: Some(2)
            }
        );
    }

    #[test]
    fn line_without_a_file_or_a_number_is_an_error() {
        assert!(err(&["--line", "3"])
            .message
            .contains("followed by the file"));
        assert!(err(&["--line", "x", "a.py"])
            .message
            .contains("positive number"));
        assert!(err(&["--line", "0", "a.py"])
            .message
            .contains("positive number"));
        assert!(err(&["a.py", "--line"]).message.contains("needs a value"));
    }

    #[test]
    fn column_without_line_is_ignored_with_a_warning() {
        let o = open(&["--column", "5", "a.py"]);
        assert_eq!(o.targets, vec![OpenTarget::new("a.py")]);
        assert_eq!(o.warnings.len(), 1);
    }

    #[test]
    fn wait_and_new_window() {
        let o = open(&["--wait", "msg.txt"]);
        assert!(o.wait && !o.new_window);
        let o = open(&["-w", "--new-window", "dir"]);
        assert!(o.wait && o.new_window);
        assert!(err(&["--wait=yes", "a"]).message.contains("takes no value"));
    }

    #[test]
    fn smoke_test_flags_keep_working() {
        let o = open(&[
            "/tmp/proj",
            "--open-file",
            "main.py",
            "--goto",
            "3:5",
            "--action",
            "SaveAll",
            "--action",
            "$Undo",
            "--tool",
            "find:TODO",
            "--terminal",
            "python3 --version",
            "--run",
            "--welcome",
            "--input-script",
            "s.txt",
            "--screenshot",
            "/tmp/s.png",
            "--verbose",
        ]);
        assert_eq!(o.targets, vec![OpenTarget::new("/tmp/proj")]);
        assert_eq!(o.open_file, Some("main.py".into()));
        assert_eq!(o.goto, Some((3, 5)));
        assert_eq!(o.actions, vec!["SaveAll".to_string(), "$Undo".to_string()]);
        assert_eq!(o.tool.as_deref(), Some("find:TODO"));
        assert_eq!(
            o.terminal.as_deref(),
            Some("python3 --version"),
            "a value that looks like an option is still the value"
        );
        assert!(o.run && o.welcome);
        assert_eq!(o.input_script, Some("s.txt".into()));
        assert_eq!(o.screenshot, Some("/tmp/s.png".into()));
        assert!(o.is_smoke_test());
        assert!(o.warnings.is_empty(), "{:?}", o.warnings);
        assert!(p(&["/tmp/proj", "--verbose"]).global.verbose);
        // A bad --goto value falls back to 1 like before.
        assert_eq!(open(&["--goto", "x:"]).goto, Some((1, 1)));
    }

    #[test]
    fn software_rendering_flag_forces_the_cpu_renderer() {
        assert!(!open(&["proj"]).software_rendering);
        let o = open(&["--software-rendering", "proj"]);
        assert!(o.software_rendering);
        assert_eq!(o.targets, vec![OpenTarget::new("proj")]);
        assert!(o.warnings.is_empty(), "{:?}", o.warnings);
        assert!(parse(["--software-rendering=yes"]).is_err());
    }

    #[test]
    fn global_options_anywhere_before_or_inside_a_command() {
        let cli = p(&[
            "--config-dir",
            "/c",
            "--system-dir=/s",
            "--log-dir",
            "/l",
            "--disable-plugins",
            "--verbose",
            "proj",
        ]);
        assert_eq!(
            cli.global,
            GlobalOptions {
                headless: false,
                config_dir: Some("/c".into()),
                system_dir: Some("/s".into()),
                log_dir: Some("/l".into()),
                disable_plugins: true,
                verbose: true
            }
        );
        assert_eq!(
            cli.command,
            Command::Open(OpenArgs {
                targets: vec![OpenTarget::new("proj")],
                ..Default::default()
            })
        );
        let cli = p(&["format", "--config-dir", "/c", "-r", "src"]);
        assert_eq!(cli.global.config_dir, Some("/c".into()));
        assert!(
            matches!(cli.command, Command::Format(ref f) if f.recursive && f.paths == vec![PathBuf::from("src")])
        );
        let cli = p(&[
            "--config-dir",
            "/c",
            "inspect",
            ".",
            "Project Default",
            "out",
        ]);
        assert_eq!(cli.global.config_dir, Some("/c".into()));
        assert!(err(&["--config-dir"]).message.contains("needs a value"));
        assert!(err(&["--config-dir="])
            .message
            .contains("needs a directory"));
    }

    #[test]
    fn dashless_launcher_arguments() {
        let cli = p(&["nosplash", "dontReopenProjects", "disableNonBundledPlugins"]);
        assert!(cli.global.disable_plugins);
        let Command::Open(o) = cli.command else {
            panic!()
        };
        assert!(o.dont_reopen_projects && o.targets.is_empty());
        // macOS process serial number.
        assert!(open(&["-psn_0_12345"]).warnings.is_empty());
    }

    #[test]
    fn unknown_options_warn_but_still_open() {
        let o = open(&["--lien", "a.py"]);
        assert_eq!(o.targets, vec![OpenTarget::new("a.py")]);
        assert_eq!(o.warnings.len(), 1);
    }

    #[test]
    fn double_dash_ends_options() {
        let o = open(&["--", "--wait", "-x"]);
        assert!(!o.wait);
        assert_eq!(
            o.targets,
            vec![OpenTarget::new("--wait"), OpenTarget::new("-x")]
        );
        assert_eq!(
            open(&["./format"]).targets,
            vec![OpenTarget::new("./format")]
        );
    }

    #[test]
    fn help_and_version() {
        assert_eq!(p(&["--help"]).command, Command::Help(None));
        assert_eq!(p(&["-h"]).command, Command::Help(None));
        assert_eq!(p(&["help"]).command, Command::Help(None));
        assert_eq!(
            p(&["help", "format"]).command,
            Command::Help(Some("format".into()))
        );
        assert_eq!(
            p(&["format", "-h"]).command,
            Command::Help(Some("format".into()))
        );
        assert_eq!(
            p(&["inspect", "-help"]).command,
            Command::Help(Some("inspect".into()))
        );
        assert_eq!(
            p(&["merge", "--help"]).command,
            Command::Help(Some("merge".into()))
        );
        assert_eq!(p(&["--version"]).command, Command::Version);
        assert!(err(&["help", "bogus"]).message.contains("no help"));
    }

    #[test]
    fn diff_and_merge() {
        assert_eq!(
            p(&["diff", "a", "b"]).command,
            Command::Diff(DiffArgs {
                left: "a".into(),
                right: "b".into(),
                ..Default::default()
            })
        );
        assert_eq!(
            p(&["diff", "a", "b", "base"]).command,
            Command::Diff(DiffArgs {
                left: "a".into(),
                right: "b".into(),
                base: Some("base".into()),
                screenshot: None
            })
        );
        assert_eq!(
            p(&["diff", "a", "--screenshot", "s.png", "b"]).command,
            Command::Diff(DiffArgs {
                left: "a".into(),
                right: "b".into(),
                base: None,
                screenshot: Some("s.png".into())
            })
        );
        assert!(err(&["diff", "a"]).message.contains("two or three"));
        assert_eq!(err(&["diff", "a", "b", "c", "d"]).command, Some("diff"));
        assert_eq!(
            p(&["merge", "l", "r", "out"]).command,
            Command::Merge(MergeArgs {
                left: "l".into(),
                right: "r".into(),
                output: "out".into(),
                ..Default::default()
            })
        );
        assert_eq!(
            p(&["merge", "l", "r", "b", "out"]).command,
            Command::Merge(MergeArgs {
                left: "l".into(),
                right: "r".into(),
                base: Some("b".into()),
                output: "out".into(),
                screenshot: None
            })
        );
        assert!(err(&["merge", "l", "r"]).message.contains("<output>"));
        assert!(err(&["diff", "-x", "a", "b"])
            .message
            .contains("unknown option"));
    }

    #[test]
    fn format_options_in_single_dash_and_gnu_spellings() {
        let Command::Format(f) = p(&[
            "format",
            "-s",
            "style.xml",
            "-R",
            "-m",
            "*.py, *.pyi",
            "-charset",
            "ISO-8859-1",
            "-dry",
            "-allowDefaults",
            "src",
            "tests/x.py",
        ])
        .command
        else {
            panic!()
        };
        assert_eq!(
            f,
            FormatArgs {
                settings: Some("style.xml".into()),
                recursive: true,
                masks: vec!["*.py".into(), "*.pyi".into()],
                charset: Some("ISO-8859-1".into()),
                dry: true,
                allow_defaults: true,
                paths: vec!["src".into(), "tests/x.py".into()]
            }
        );
        let Command::Format(f) = p(&[
            "format",
            "-settings",
            "s.json",
            "-r",
            "-mask",
            "*.py",
            "-mask",
            "bin/*",
            "-d",
            ".",
        ])
        .command
        else {
            panic!()
        };
        assert_eq!(f.settings, Some("s.json".into()));
        assert_eq!(f.masks, vec!["*.py".to_string(), "bin/*".to_string()]);
        assert!(f.dry && f.recursive);
        let Command::Format(f) = p(&[
            "format",
            "--settings=s.json",
            "--dry-run",
            "--allow-defaults",
            "a.py",
        ])
        .command
        else {
            panic!()
        };
        assert!(f.dry && f.allow_defaults && f.settings == Some("s.json".into()));
        assert!(err(&["format"]).message.contains("at least one"));
        assert!(err(&["format", "-x", "a.py"])
            .message
            .contains("unknown option"));
        assert!(err(&["format", "a.py", "-s"])
            .message
            .contains("needs a value"));
    }

    #[test]
    fn inspect_arguments() {
        let Command::Inspect(i) = p(&[
            "inspect",
            "proj",
            "Project Default",
            "out",
            "-d",
            "proj/src",
            "-changes",
            "-format",
            "sarif",
            "-v2",
        ])
        .command
        else {
            panic!()
        };
        assert_eq!(
            i,
            InspectArgs {
                project: "proj".into(),
                profile: "Project Default".into(),
                output: "out".into(),
                dir: Some("proj/src".into()),
                changes: true,
                format: ReportFormat::Sarif,
                verbosity: 2
            }
        );
        let Command::Inspect(i) = p(&[
            "inspect",
            "proj",
            "out",
            "-profileName",
            "Strict",
            "-v0",
            "-format",
            "json",
        ])
        .command
        else {
            panic!()
        };
        assert_eq!(
            (i.profile.as_str(), i.output, i.verbosity, i.format),
            ("Strict", PathBuf::from("out"), 0, ReportFormat::Json)
        );
        let Command::Inspect(i) = p(&["inspect", "proj", "p.xml", "out"]).command else {
            panic!()
        };
        assert_eq!(
            (i.format, i.verbosity, i.changes, i.dir),
            (ReportFormat::Xml, 1, false, None)
        );
        assert!(err(&["inspect", "proj", "out"])
            .message
            .contains("<profile>"));
        assert!(err(&["inspect", "proj", "p", "out", "-format", "html"])
            .message
            .contains("unknown report format"));
        assert!(err(&["inspect", "a", "b", "c", "-profileName", "x"])
            .message
            .contains("not both"));
    }

    #[test]
    fn completions_and_launcher() {
        assert_eq!(
            p(&["completions", "zsh"]).command,
            Command::Completions(Shell::Zsh)
        );
        assert_eq!(
            p(&["completions", "pwsh"]).command,
            Command::Completions(Shell::PowerShell)
        );
        assert!(err(&["completions", "tcsh"])
            .message
            .contains("unknown shell"));
        assert!(err(&["completions"]).message.contains("needs a shell"));
        assert_eq!(
            p(&["install-launcher"]).command,
            Command::InstallLauncher(LauncherArgs::default())
        );
        assert_eq!(
            p(&["install-launcher", "/usr/local/bin", "--force"]).command,
            Command::InstallLauncher(LauncherArgs {
                path: Some("/usr/local/bin".into()),
                force: true
            })
        );
        assert!(err(&["install-launcher", "a", "b"])
            .message
            .contains("at most one"));
    }
}

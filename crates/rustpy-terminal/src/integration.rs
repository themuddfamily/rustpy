//! Bounded incremental OSC integration; sequences may cross PTY read boundaries.
use std::{collections::BTreeMap, path::PathBuf};
#[derive(Clone, Debug, Default)]
pub struct CommandBlock {
    pub id: u64,
    pub command: String,
    pub output: String,
    pub status: Option<i32>,
}
#[derive(Clone, Debug, Default)]
pub struct Metadata {
    pub title: String,
    pub cwd: Option<PathBuf>,
    pub cursor_shape: u8,
    pub bells: u64,
    pub hyperlinks: BTreeMap<String, String>,
    pub clipboard: Option<String>,
    pub blocks: Vec<CommandBlock>,
    pub prompt: bool,
    pub command_running: bool,
}
#[derive(Default)]
pub struct Tracker {
    pub metadata: Metadata,
    state: u8,
    control: Vec<u8>,
    active_link: Option<(String, Vec<u8>)>,
    next_block: u64,
    output_pending: Vec<u8>,
}
fn percent_decode(text: &str) -> String {
    let mut bytes = Vec::new();
    let mut i = 0;
    let raw = text.as_bytes();
    while i < raw.len() {
        if raw[i] == b'%' && i + 2 < raw.len() {
            if let Some(v) = std::str::from_utf8(&raw[i + 1..i + 3])
                .ok()
                .and_then(|s| u8::from_str_radix(s, 16).ok())
            {
                bytes.push(v);
                i += 3;
                continue;
            }
        }
        bytes.push(raw[i]);
        i += 1;
    }
    String::from_utf8_lossy(&bytes).into_owned()
}
impl Tracker {
    pub fn command(&mut self, command: &str) {
        if self.metadata.command_running {
            return;
        }
        self.next_block += 1;
        self.metadata.blocks.push(CommandBlock {
            id: self.next_block,
            command: command.chars().take(8192).collect(),
            ..Default::default()
        });
        if self.metadata.blocks.len() > 200 {
            self.metadata.blocks.remove(0);
        }
        self.metadata.command_running = true;
    }
    pub fn process(&mut self, bytes: &[u8]) {
        for &b in bytes {
            match self.state {
                0 => match b {
                    0x1b => self.state = 1,
                    7 => self.metadata.bells += 1,
                    _ => {
                        if let Some((_, text)) = &mut self.active_link {
                            if text.len() < 8192 {
                                text.push(b);
                            }
                        }
                        if self.metadata.command_running
                            && (b >= 32 || b == b'\n' || b == b'\r' || b == b'\t')
                        {
                            if let Some(block) = self.metadata.blocks.last_mut() {
                                if block.output.len() < 65536 {
                                    self.output_pending.push(b);
                                    match std::str::from_utf8(&self.output_pending) {
                                        Ok(text) => {
                                            block.output.push_str(text);
                                            self.output_pending.clear();
                                        }
                                        Err(error) if error.error_len().is_some() => {
                                            block.output.push('\u{fffd}');
                                            self.output_pending.clear();
                                        }
                                        _ => {}
                                    }
                                }
                            }
                        }
                    }
                },
                1 => {
                    self.control.clear();
                    self.state = match b {
                        b']' => 2,
                        b'[' => 4,
                        _ => 0,
                    };
                }
                2 => {
                    if b == 7 {
                        self.osc();
                        self.state = 0;
                    } else if b == 0x1b {
                        self.state = 3;
                    } else if self.control.len() < 65536 {
                        self.control.push(b);
                    } else {
                        self.state = 0;
                    }
                }
                3 => {
                    if b == b'\\' {
                        self.osc();
                        self.state = 0;
                    } else {
                        self.state = 2;
                    }
                }
                4 => {
                    if (0x40..=0x7e).contains(&b) {
                        if b == b'q' {
                            self.metadata.cursor_shape = String::from_utf8_lossy(&self.control)
                                .trim()
                                .parse()
                                .unwrap_or(0);
                        }
                        self.state = 0;
                    } else if self.control.len() < 128 {
                        self.control.push(b);
                    } else {
                        self.state = 0;
                    }
                }
                _ => self.state = 0,
            }
        }
    }
    fn osc(&mut self) {
        let text = String::from_utf8_lossy(&self.control).into_owned();
        let Some((code, value)) = text.split_once(';') else {
            return;
        };
        match code {
            "0" | "2" => {
                self.metadata.title = value
                    .chars()
                    .filter(|c| !c.is_control())
                    .take(160)
                    .collect()
            }
            "7" => {
                if let Some(rest) = value.strip_prefix("file://") {
                    if let Some(slash) = rest.find('/') {
                        let path = percent_decode(&rest[slash..]);
                        if !path.contains('\0') {
                            self.metadata.cwd = Some(path.into());
                        }
                    }
                }
            }
            "8" => {
                if let Some((_, url)) = value.split_once(';') {
                    if let Some((old, label)) = self.active_link.take() {
                        let label = String::from_utf8_lossy(&label).into_owned();
                        if !label.is_empty() && self.metadata.hyperlinks.len() < 1024 {
                            self.metadata.hyperlinks.insert(label, old);
                        }
                    }
                    if url.starts_with("https://")
                        || url.starts_with("http://")
                        || url.starts_with("file://")
                    {
                        self.active_link = Some((url.into(), Vec::new()));
                    }
                }
            }
            "52" => {
                if let Some((_, encoded)) = value.split_once(';') {
                    if encoded != "?" {
                        self.metadata.clipboard = decode_base64(encoded);
                    }
                }
            }
            "133" => {
                let (phase, extra) = value.split_once(';').unwrap_or((value, ""));
                match phase {
                    "A" => {
                        self.metadata.prompt = true;
                        self.metadata.command_running = false;
                    }
                    "B" => self.metadata.prompt = false,
                    "C" => {
                        self.command(&percent_decode(extra));
                    }
                    "D" => {
                        if let Some(block) = self.metadata.blocks.last_mut() {
                            block.status = extra.parse().ok();
                        }
                        self.metadata.command_running = false;
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
}
fn decode_base64(value: &str) -> Option<String> {
    if value.len() > 65536 {
        return None;
    }
    let mut bits = 0u32;
    let mut count = 0;
    let mut out = Vec::new();
    for b in value.bytes().take_while(|b| *b != b'=') {
        let v = match b {
            b'A'..=b'Z' => b - b'A',
            b'a'..=b'z' => b - b'a' + 26,
            b'0'..=b'9' => b - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        };
        bits = (bits << 6) | u32::from(v);
        count += 6;
        if count >= 8 {
            count -= 8;
            out.push((bits >> count) as u8);
            bits &= (1 << count) - 1;
        }
    }
    String::from_utf8(out).ok()
}
pub fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}
fn shell_name(shell: &str) -> Option<String> {
    Some(
        std::path::Path::new(shell)
            .file_stem()?
            .to_string_lossy()
            .to_ascii_lowercase(),
    )
}
/// OSC 133 / OSC 7 hooks for `name`; `prefix` is a shell expression for the prompt prefix.
fn hooks(name: &str, prefix: &str) -> Option<String> {
    Some(match name {
        "bash"=>format!("__rustpy_prompt() {{ local s=$?; printf '\\033]133;D;%s\\007\\033]7;file://localhost%s\\007\\033]133;A\\007' \"$s\" \"$PWD\"; }}; PROMPT_COMMAND=\"__rustpy_prompt${{PROMPT_COMMAND:+; $PROMPT_COMMAND}}\"; PS1={prefix}\"$PS1\"; trap 'printf \"\\033]133;C;\\007\"' DEBUG"),
        "zsh"=>format!("autoload -Uz add-zsh-hook; __rustpy_precmd() {{ printf '\\033]133;D;%s\\007\\033]7;file://localhost%s\\007\\033]133;A\\007' \"$?\" \"$PWD\"; }}; __rustpy_preexec() {{ printf '\\033]133;C;\\007'; }}; add-zsh-hook precmd __rustpy_precmd; add-zsh-hook preexec __rustpy_preexec; PS1={prefix}\"$PS1\""),
        "fish"=>"function __rustpy_prompt --on-event fish_prompt; printf '\\033]133;D;%s\\007\\033]7;file://localhost%s\\007\\033]133;A\\007' $status $PWD; end; function __rustpy_preexec --on-event fish_preexec; printf '\\033]133;C;\\007'; end".into(),
        "pwsh"|"powershell"=>"$global:__rustpy_old_prompt = $function:prompt; function global:prompt { [Console]::Write(\"`e]133;D;$LASTEXITCODE`a`e]7;file://localhost$($pwd.Path.Replace('\\','/'))`a`e]133;A`a\"); & $global:__rustpy_old_prompt }".into(),
        _=>return None,
    })
}
/// Typed-input fallback: hooks for the interactive shell followed by a screen clear,
/// so the echoed bootstrap does not stay visible. Existing prompt hooks are retained.
pub fn shell_setup(shell: &str, prefix: &str) -> Option<String> {
    let name = shell_name(shell)?;
    let result = hooks(&name, &quote(prefix))?;
    let clear = if matches!(name.as_str(), "pwsh" | "powershell") {
        "Clear-Host"
    } else {
        "clear"
    };
    Some(format!(" {result}; {clear}\r"))
}
/// How a local shell is launched with integration injected non-interactively
/// (as VS Code does): extra argv and environment for the process.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Launch {
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
}
/// Environment variable carrying the prompt prefix into the generated startup files.
pub const PREFIX_VAR: &str = "RUSTPY_PROMPT_PREFIX";
/// Environment variable carrying a startup command (venv/conda activation) to run
/// after the user's own startup files.
pub const STARTUP_VAR: &str = "RUSTPY_STARTUP";
const BASH_RC: &str = r#"# RustPy shell integration (generated)
if [ -n "$RUSTPY_LOGIN_SHELL" ]; then
  unset RUSTPY_LOGIN_SHELL
  [ -r /etc/profile ] && . /etc/profile
  if [ -r ~/.bash_profile ]; then . ~/.bash_profile
  elif [ -r ~/.bash_login ]; then . ~/.bash_login
  elif [ -r ~/.profile ]; then . ~/.profile
  fi
else
  [ -r ~/.bashrc ] && . ~/.bashrc
fi
if [ -n "$RUSTPY_STARTUP" ]; then eval "$RUSTPY_STARTUP"; fi
unset RUSTPY_STARTUP
"#;
const ZSH_ENV: &str = r#"# RustPy shell integration (generated)
__rustpy_zdotdir="$ZDOTDIR"
ZDOTDIR="${RUSTPY_USER_ZDOTDIR:-$HOME}"
[ -r "$ZDOTDIR/.zshenv" ] && . "$ZDOTDIR/.zshenv"
RUSTPY_USER_ZDOTDIR="$ZDOTDIR"
ZDOTDIR="$__rustpy_zdotdir"
"#;
const ZSH_PROFILE: &str = r#"# RustPy shell integration (generated)
ZDOTDIR="$RUSTPY_USER_ZDOTDIR"
[ -r "$ZDOTDIR/.zprofile" ] && . "$ZDOTDIR/.zprofile"
ZDOTDIR="$__rustpy_zdotdir"
"#;
const ZSH_RC: &str = r#"# RustPy shell integration (generated)
ZDOTDIR="$RUSTPY_USER_ZDOTDIR"
unset RUSTPY_USER_ZDOTDIR
[ -r "$ZDOTDIR/.zshrc" ] && . "$ZDOTDIR/.zshrc" __rustpy_zdotdir
if [ -n "$RUSTPY_STARTUP" ]; then eval "$RUSTPY_STARTUP"; fi
unset RUSTPY_STARTUP
"#;
fn write_if_changed(path: &std::path::Path, content: &str) -> std::io::Result<()> {
    if std::fs::read_to_string(path).ok().as_deref() != Some(content) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, content)?;
    }
    Ok(())
}
/// Writes the integration startup files for `shell` under `dir` and returns how to
/// launch it, or `None` when the shell or its arguments are not supported (callers
/// then fall back to [`shell_setup`]). The user's own startup files are sourced
/// first; `startup` runs after them, and the hooks are installed last.
pub fn launch(
    shell: &str,
    args: &[String],
    prefix: &str,
    startup: Option<&str>,
    dir: &std::path::Path,
) -> std::io::Result<Option<Launch>> {
    let Some(name) = shell_name(shell) else {
        return Ok(None);
    };
    // Shells without prompt-prefix support ignore the expression.
    let Some(hooks) = hooks(&name, &format!("\"${PREFIX_VAR}\"")) else {
        return Ok(None);
    };
    let mut env = vec![(PREFIX_VAR.to_string(), prefix.to_string())];
    if let Some(startup) = startup.filter(|s| !s.is_empty()) {
        env.push((STARTUP_VAR.to_string(), startup.to_string()));
    }
    let unset_prefix = format!("unset {PREFIX_VAR}");
    let launch = match name.as_str() {
        "bash" => {
            // A login shell ignores --rcfile, so login is emulated by the rc file.
            if !args
                .iter()
                .all(|a| matches!(a.as_str(), "-l" | "--login" | "-i"))
            {
                return Ok(None);
            }
            if args.iter().any(|a| a == "-l" || a == "--login") {
                env.push(("RUSTPY_LOGIN_SHELL".into(), "1".into()));
            }
            let rc = dir.join("bash").join("rustpy.bashrc");
            write_if_changed(&rc, &format!("{BASH_RC}{hooks}\n{unset_prefix}\n"))?;
            Launch {
                args: vec![
                    "--rcfile".into(),
                    rc.to_string_lossy().into_owned(),
                    "-i".into(),
                ],
                env,
            }
        }
        "zsh" => {
            if !args
                .iter()
                .all(|a| matches!(a.as_str(), "-l" | "--login" | "-i"))
            {
                return Ok(None);
            }
            let zdotdir = dir.join("zsh");
            write_if_changed(&zdotdir.join(".zshenv"), ZSH_ENV)?;
            write_if_changed(&zdotdir.join(".zprofile"), ZSH_PROFILE)?;
            // .zshrc leaves the user's ZDOTDIR in place, so .zlogin and later
            // lookups behave normally.
            write_if_changed(
                &zdotdir.join(".zshrc"),
                &format!("{ZSH_RC}{hooks}\n{unset_prefix}\n"),
            )?;
            if let Some(user) = std::env::var_os("ZDOTDIR").filter(|d| !d.is_empty()) {
                env.push((
                    "RUSTPY_USER_ZDOTDIR".into(),
                    user.to_string_lossy().into_owned(),
                ));
            }
            env.push(("ZDOTDIR".into(), zdotdir.to_string_lossy().into_owned()));
            Launch {
                args: args.to_vec(),
                env,
            }
        }
        "fish" => {
            if args
                .iter()
                .any(|a| a == "-c" || a.starts_with("--command") || a == "-n")
            {
                return Ok(None);
            }
            let script = dir.join("fish").join("rustpy.fish");
            write_if_changed(
                &script,
                &format!("# RustPy shell integration (generated)\nif set -q {STARTUP_VAR}; eval ${STARTUP_VAR}; set -e {STARTUP_VAR}; end\n{hooks}\nset -e {PREFIX_VAR}\n"),
            )?;
            let mut args = args.to_vec();
            args.push("--init-command".into());
            args.push(format!("source {}", quote(&script.to_string_lossy())));
            Launch { args, env }
        }
        "pwsh" | "powershell" => {
            if !args.is_empty() {
                return Ok(None);
            }
            let script = dir.join("powershell").join("rustpy.ps1");
            write_if_changed(
                &script,
                &format!("# RustPy shell integration (generated)\nif ($env:{STARTUP_VAR}) {{ Invoke-Expression $env:{STARTUP_VAR}; Remove-Item Env:{STARTUP_VAR} }}\n{hooks}\nRemove-Item Env:{PREFIX_VAR} -ErrorAction SilentlyContinue\n"),
            )?;
            let path = script.to_string_lossy().replace('\'', "''");
            Launch {
                args: vec![
                    "-NoLogo".into(),
                    "-NoExit".into(),
                    "-Command".into(),
                    format!(". '{path}'"),
                ],
                env,
            }
        }
        _ => return Ok(None),
    };
    Ok(Some(launch))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn split_osc_and_clipboard_and_links() {
        let mut t = Tracker::default();
        t.process(b"\x1b]7;file://localhost/a%20b\x1b");
        t.process(b"\\\x1b]8;;https://example.org\x07label\x1b]8;;\x07\x1b]52;c;aGVsbG8=\x07");
        assert_eq!(t.metadata.cwd, Some("/a b".into()));
        assert_eq!(t.metadata.hyperlinks["label"], "https://example.org");
        assert_eq!(t.metadata.clipboard.as_deref(), Some("hello"));
    }
    #[test]
    fn actual_command_phases_bound_output() {
        let mut t = Tracker::default();
        t.process(b"\x1b]133;C;pytest%20-q\x07output\x1b]133;D;1\x07");
        assert_eq!(t.metadata.blocks[0].command, "pytest -q");
        assert_eq!(t.metadata.blocks[0].status, Some(1));
        assert_eq!(t.metadata.blocks[0].output, "output");
    }
}
#[cfg(test)]
mod launch_tests {
    use super::*;
    use std::path::PathBuf;
    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("rustpy-integration-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }
    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|v| v.to_string()).collect()
    }
    #[test]
    #[cfg(unix)] // Unix fixture paths, tools or process semantics
    fn bash_uses_generated_rcfile_and_emulates_login() {
        let dir = scratch("bash");
        let bash = launch(
            "/bin/bash",
            &args(&["-l"]),
            "> ",
            Some(". venv/bin/activate"),
            &dir,
        )
        .unwrap()
        .unwrap();
        let rc = dir.join("bash/rustpy.bashrc");
        assert_eq!(bash.args, args(&["--rcfile", &rc.to_string_lossy(), "-i"]));
        assert!(bash
            .env
            .contains(&("RUSTPY_LOGIN_SHELL".into(), "1".into())));
        assert!(bash.env.contains(&(PREFIX_VAR.into(), "> ".into())));
        assert!(bash
            .env
            .contains(&(STARTUP_VAR.into(), ". venv/bin/activate".into())));
        let text = std::fs::read_to_string(&rc).unwrap();
        assert!(text.contains(". ~/.bashrc") && text.contains(". ~/.bash_profile"));
        assert!(text.contains("133;A") && text.contains("PROMPT_COMMAND"));
        // Without -l the user's ~/.bashrc path is taken; no login marker.
        let plain = launch("bash", &[], "", None, &dir).unwrap().unwrap();
        assert!(!plain.env.iter().any(|(k, _)| k == "RUSTPY_LOGIN_SHELL"));
        assert!(!plain.env.iter().any(|(k, _)| k == STARTUP_VAR));
        // Custom arguments (e.g. -c) cannot be combined with --rcfile: typed fallback.
        assert_eq!(
            launch("bash", &args(&["-c", "true"]), "", None, &dir).unwrap(),
            None
        );
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn generated_bashrc_sources_user_rc_then_installs_hooks() {
        let Ok(program) = which_bash() else { return };
        let dir = scratch("bashrun");
        let home = dir.join("home");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(
            home.join(".bashrc"),
            "USER_RC=yes\nPROMPT_COMMAND=user_hook\n",
        )
        .unwrap();
        let generated = launch(
            &program,
            &[],
            "P> ",
            Some("STARTED=$USER_RC"),
            &dir.join("gen"),
        )
        .unwrap()
        .unwrap();
        let mut child = std::process::Command::new(&program)
            .args(&generated.args)
            .envs(generated.env.iter().cloned())
            .env("HOME", &home)
            .env("PS1", "$ ")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        use std::io::Write as _;
        child
            .stdin
            .take()
            .unwrap()
            .write_all(b"echo \"[$USER_RC|$STARTED|$PROMPT_COMMAND|$PS1|${RUSTPY_STARTUP-unset}|${RUSTPY_PROMPT_PREFIX-unset}]\"\nexit\n")
            .unwrap();
        let out = child.wait_with_output().unwrap();
        let out = String::from_utf8_lossy(&out.stdout);
        // The system bashrc may replace PS1; the prefix still leads it.
        assert!(
            out.contains("[yes|yes|__rustpy_prompt; user_hook|P> "),
            "{out}"
        );
        assert!(out.contains("|unset|unset]"), "{out}");
        std::fs::remove_dir_all(dir).unwrap();
    }
    fn which_bash() -> Result<String, ()> {
        ["/bin/bash", "/usr/bin/bash"]
            .into_iter()
            .find(|p| std::path::Path::new(p).is_file())
            .map(String::from)
            .ok_or(())
    }
    #[test]
    fn zsh_uses_generated_zdotdir_that_sources_user_files() {
        let dir = scratch("zsh");
        let zsh = launch("/usr/bin/zsh", &args(&["-l"]), "", None, &dir)
            .unwrap()
            .unwrap();
        assert_eq!(zsh.args, args(&["-l"]));
        let zdotdir = dir.join("zsh");
        assert!(zsh
            .env
            .contains(&("ZDOTDIR".into(), zdotdir.to_string_lossy().into_owned())));
        for (file, user) in [
            (".zshenv", ".zshenv"),
            (".zprofile", ".zprofile"),
            (".zshrc", ".zshrc"),
        ] {
            let text = std::fs::read_to_string(zdotdir.join(file)).unwrap();
            assert!(text.contains(&format!("$ZDOTDIR/{user}")), "{file}");
        }
        let rc = std::fs::read_to_string(zdotdir.join(".zshrc")).unwrap();
        assert!(rc.find(".zshrc").unwrap() < rc.find("add-zsh-hook precmd").unwrap());
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    #[cfg(unix)] // Unix fixture paths, tools or process semantics
    fn fish_and_powershell_load_integration_after_user_config() {
        let dir = scratch("fish");
        let fish = launch("fish", &args(&["-l"]), "", None, &dir)
            .unwrap()
            .unwrap();
        let script = dir.join("fish/rustpy.fish");
        assert_eq!(
            fish.args,
            args(&[
                "-l",
                "--init-command",
                &format!("source {}", quote(&script.to_string_lossy()))
            ])
        );
        assert!(std::fs::read_to_string(&script)
            .unwrap()
            .contains("fish_preexec"));
        let pwsh = launch("pwsh", &[], "", None, &dir).unwrap().unwrap();
        assert_eq!(
            &pwsh.args[..3],
            &args(&["-NoLogo", "-NoExit", "-Command"])[..]
        );
        assert!(pwsh.args[3].starts_with(". '") && pwsh.args[3].ends_with("rustpy.ps1'"));
        assert!(
            launch("powershell.exe", &args(&["-NoProfile"]), "", None, &dir)
                .unwrap()
                .is_none()
        );
        assert!(launch("/bin/sh", &[], "", None, &dir).unwrap().is_none());
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn typed_fallback_clears_the_echoed_bootstrap() {
        let bash = shell_setup("/bin/bash", "> ").unwrap();
        assert!(bash.starts_with(' ') && bash.ends_with("; clear\r"));
        assert!(shell_setup("pwsh", "").unwrap().ends_with("; Clear-Host\r"));
        assert!(shell_setup("/bin/sh", "").is_none());
    }
}
#[cfg(test)]
mod regression_fixtures {
    use super::*;
    #[test]
    fn unicode_output_and_malformed_percent_are_safe() {
        let mut tracker = Tracker::default();
        tracker.process(b"\x1b]133;C;echo\x07");
        for byte in "héλ".as_bytes() {
            tracker.process(&[*byte]);
        }
        assert_eq!(tracker.metadata.blocks[0].output, "héλ");
        assert_eq!(percent_decode("%é"), "%é");
    }
    #[test]
    fn bounded_unknown_osc_does_not_capture_forever() {
        let mut tracker = Tracker::default();
        tracker.process(b"\x1b]999;");
        tracker.process(&vec![b'x'; 70000]);
        tracker.process(b"\x1b]2;Recovered\x07\x1b[5 q");
        assert_eq!(tracker.metadata.title, "Recovered");
        assert_eq!(tracker.metadata.cursor_shape, 5);
    }
    #[test]
    fn frontend_command_and_shell_start_form_one_block() {
        let mut tracker = Tracker::default();
        tracker.command("pip install example");
        tracker.process(b"\x1b]133;C;\x07done\x1b]133;D;0\x07");
        assert_eq!(tracker.metadata.blocks.len(), 1);
        assert_eq!(tracker.metadata.blocks[0].command, "pip install example");
        assert_eq!(tracker.metadata.blocks[0].status, Some(0));
    }
}

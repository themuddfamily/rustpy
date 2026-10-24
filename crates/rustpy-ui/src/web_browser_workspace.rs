//! Pure browser launch and URL planning, shared by editor and Open URL consumers.
use rustpy_settings::web_browsers::{Kind, Profile};
use std::path::{Path, PathBuf};
#[derive(Debug, PartialEq, Eq)]
pub enum Launch {
    System(String),
    Program { program: PathBuf, args: Vec<String> },
}
pub fn validated_url(input: &str) -> Result<String, String> {
    if input.chars().any(char::is_control) {
        return Err("URLs cannot contain control characters".into());
    }
    let url = url::Url::parse(input.trim()).map_err(|e| format!("Invalid URL: {e}"))?;
    if !matches!(url.scheme(), "http" | "https" | "file") {
        return Err("Use an http, https or file URL".into());
    }
    if url.username() != "" || url.password().is_some() {
        return Err("Do not include credentials in browser URLs".into());
    }
    if matches!(url.scheme(), "http" | "https") && url.host_str().is_none() {
        return Err("URL needs a host".into());
    }
    Ok(url.into())
}
pub fn launch(profile: &Profile, url: &str) -> Result<Launch, String> {
    let url = validated_url(url)?;
    if !profile.enabled {
        return Err("Browser profile is disabled".into());
    }
    if profile.kind == Kind::System {
        return Ok(Launch::System(url));
    }
    if profile.program.trim().is_empty() {
        return Err(format!("Configure the executable for {}", profile.name));
    }
    // Split the template before substituting URL: spaces/metacharacters in a
    // URL cannot become another option or shell command.
    let mut had_url = false;
    let mut args = crate::external_tools::split_template(&profile.arguments)
        .into_iter()
        .map(|(arg, _)| {
            had_url |= arg.contains("$URL$");
            arg.replace("$URL$", &url)
        })
        .collect::<Vec<_>>();
    if !had_url {
        args.push(url);
    }
    let program = PathBuf::from(&profile.program);
    #[cfg(all(unix, not(target_os = "macos")))]
    let program = detect_program(profile).unwrap_or(program);
    #[cfg(windows)]
    let program = detect_program(profile).unwrap_or(program);
    Ok(Launch::Program { program, args })
}
/// Linux/BSD package names differ by vendor and channel: Edge installs
/// `microsoft-edge-stable` (the `microsoft-edge` alternative is optional),
/// Debian ships `firefox-esr`, Fedora/Ubuntu ship `chromium(-browser)`. A bare
/// configured name of a known browser kind is resolved on PATH, then against
/// those channel names. Absolute or path-qualified programs are used verbatim.
#[cfg(all(unix, not(target_os = "macos")))]
pub fn detect_program(profile: &Profile) -> Option<PathBuf> {
    detect_program_in(profile, &std::env::var_os("PATH")?)
}
#[cfg(all(unix, not(target_os = "macos")))]
fn detect_program_in(profile: &Profile, path: &std::ffi::OsStr) -> Option<PathBuf> {
    let configured = profile.program.trim();
    if configured.is_empty() || configured.contains('/') {
        return None;
    }
    let alternates: &[&str] = match profile.kind {
        Kind::Chrome => &[
            "google-chrome",
            "google-chrome-stable",
            "chromium",
            "chromium-browser",
        ],
        Kind::Edge => &[
            "microsoft-edge",
            "microsoft-edge-stable",
            "microsoft-edge-beta",
            "microsoft-edge-dev",
        ],
        Kind::Firefox => &["firefox", "firefox-esr"],
        _ => &[],
    };
    let find = |name: &str| {
        use std::os::unix::fs::PermissionsExt;
        std::env::split_paths(path)
            .map(|dir| dir.join(name))
            .find(|candidate| {
                std::fs::metadata(candidate)
                    .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
            })
    };
    std::iter::once(configured)
        .chain(
            alternates
                .iter()
                .copied()
                .filter(|name| *name != configured),
        )
        .find_map(find)
}
/// Windows browsers are rarely on PATH and `CreateProcess` ignores the App
/// Paths registrations Explorer uses. A bare configured name (`msedge.exe`,
/// `chrome`, a custom `brave.exe`) resolves through the vendor's per-user and
/// per-machine install folders, then App Paths (HKCU before HKLM), then PATH.
/// Absolute or path-qualified programs are used verbatim.
#[cfg(windows)]
pub fn detect_program(profile: &Profile) -> Option<PathBuf> {
    let configured = profile.program.trim().trim_matches('"');
    if configured.is_empty() || configured.contains(['/', '\\', ':']) {
        return None;
    }
    // Joined one component at a time: a `/` inside a Windows path survives
    // into child command lines, where tools read it as a switch.
    let relative: Option<&[&str]> = match profile.kind {
        Kind::Chrome => Some(&["Google", "Chrome", "Application", "chrome.exe"]),
        Kind::Firefox => Some(&["Mozilla Firefox", "firefox.exe"]),
        Kind::Edge => Some(&["Microsoft", "Edge", "Application", "msedge.exe"]),
        _ => None,
    };
    if let Some(relative) = relative {
        for base in [
            "LOCALAPPDATA",
            "PROGRAMFILES",
            "PROGRAMFILES(X86)",
            "PROGRAMW6432",
        ] {
            if let Some(base) = std::env::var_os(base) {
                let path: PathBuf = std::iter::once(PathBuf::from(base))
                    .chain(relative.iter().map(PathBuf::from))
                    .collect();
                if path.is_file() {
                    return Some(path);
                }
            }
        }
    }
    let image = if configured.to_ascii_lowercase().ends_with(".exe") {
        configured.to_string()
    } else {
        format!("{configured}.exe")
    };
    let reg = reg_exe();
    for hive in ["HKCU", "HKLM"] {
        let key =
            format!("{hive}\\Software\\Microsoft\\Windows\\CurrentVersion\\App Paths\\{image}");
        let Some(output) = rustpy_project::probe::probe_output(
            &reg,
            &["query", &key, "/ve"],
            std::time::Duration::from_secs(3),
        ) else {
            continue;
        };
        if let Some(path) = app_paths_default(&String::from_utf8_lossy(&output), |name| {
            std::env::var(name).ok()
        }) {
            if path.is_absolute() && path.is_file() {
                return Some(path);
            }
        }
    }
    rustpy_project::setup::find_on_path(&image[..image.len() - 4])
}
#[cfg(not(any(windows, all(unix, not(target_os = "macos")))))]
pub fn detect_program(_profile: &Profile) -> Option<PathBuf> {
    None
}
#[cfg(windows)]
fn reg_exe() -> PathBuf {
    std::env::var_os("SystemRoot")
        .map(|root| PathBuf::from(root).join("System32").join("reg.exe"))
        .filter(|reg| reg.is_file())
        .unwrap_or_else(|| PathBuf::from("reg.exe"))
}
/// What the settings page shows for a browser entry: the resolved program
/// and, on Windows, the Image File Execution Options `Debugger` that Windows
/// starts instead of it (MSEdgeRedirect registers one for `msedge.exe`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Detected {
    pub program: Option<PathBuf>,
    pub redirected_by: Option<String>,
}
pub fn detect(profile: &Profile) -> Detected {
    let program = detect_program(profile);
    let configured = PathBuf::from(profile.program.trim().trim_matches('"'));
    let redirected_by = redirected_by(program.as_deref().unwrap_or(&configured));
    Detected {
        program,
        redirected_by,
    }
}
/// [`detect`] for per-frame UI: Windows detection runs `reg.exe`, so results
/// are kept per (kind, configured program) for the session there.
pub fn detect_cached(profile: &Profile) -> Detected {
    if !cfg!(windows) {
        return detect(profile);
    }
    type Cache = std::sync::Mutex<std::collections::HashMap<(String, String), Detected>>;
    static CACHE: std::sync::LazyLock<Cache> = std::sync::LazyLock::new(Default::default);
    let key = (format!("{:?}", profile.kind), profile.program.clone());
    if let Some(found) = CACHE.lock().ok().and_then(|cache| cache.get(&key).cloned()) {
        return found;
    }
    let found = detect(profile);
    if let Ok(mut cache) = CACHE.lock() {
        cache.insert(key, found.clone());
    }
    found
}
/// The Image File Execution Options `Debugger` Windows runs in place of
/// `program` (matched by image name, e.g. `msedge.exe`): `CreateProcess`
/// then starts `<debugger> "<program>" <args>` instead. That is a deliberate
/// system setting (MSEdgeRedirect hands Edge launches to the default
/// browser); RustPy respects it and reports it.
#[cfg(windows)]
pub fn redirected_by(program: &Path) -> Option<String> {
    let image = program.file_name()?.to_str()?;
    let image = if image.to_ascii_lowercase().ends_with(".exe") {
        image.to_string()
    } else {
        format!("{image}.exe")
    };
    let key = format!(
        "HKLM\\SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion\\Image File Execution Options\\{image}"
    );
    let output = rustpy_project::probe::probe_output(
        &reg_exe(),
        &["query", &key, "/s"],
        std::time::Duration::from_secs(3),
    )?;
    ifeo_debugger(&String::from_utf8_lossy(&output), program)
}
#[cfg(not(windows))]
pub fn redirected_by(_program: &Path) -> Option<String> {
    None
}
/// The message shown when `browser`'s launches are redirected by `debugger`.
pub fn redirect_message(browser: &str, debugger: &str) -> String {
    format!(
        "{browser} launches are redirected by {} (Image File Execution Options); \
         RustPy cannot open pages in {browser} on this machine.",
        debugger_name(debugger)
    )
}
/// The file name of the program in an IFEO `Debugger` command line, which
/// may be quoted and carry arguments.
pub fn debugger_name(debugger: &str) -> String {
    let debugger = debugger.trim();
    let program = if let Some(rest) = debugger.strip_prefix('"') {
        rest.split('"').next().unwrap_or(rest)
    } else {
        match debugger.to_ascii_lowercase().find(".exe") {
            Some(end) => &debugger[..end + 4],
            None => debugger.split_whitespace().next().unwrap_or(debugger),
        }
    };
    program
        .rsplit(['\\', '/'])
        .next()
        .unwrap_or(program)
        .to_string()
}
/// The `Debugger` of an IFEO image key as `reg query <key> /s` prints it.
/// With a non-zero `UseFilter` on the image key, a direct subkey whose
/// `FilterFullPath` names `program` applies first; otherwise (or when no
/// filter matches) the image key's own `Debugger` does.
#[cfg_attr(not(windows), allow(dead_code))]
fn ifeo_debugger(output: &str, program: &Path) -> Option<String> {
    let mut keys: Vec<(&str, Vec<(&str, &str)>)> = Vec::new();
    for line in output.lines() {
        if line.starts_with("HKEY_") {
            keys.push((line.trim(), Vec::new()));
        } else if let (Some((_, values)), Some(value)) = (keys.last_mut(), reg_value(line)) {
            values.push(value);
        }
    }
    let value = |values: &[(&str, &str)], name: &str| {
        values
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, data)| data.trim_matches('\0').trim())
            .filter(|data| !data.is_empty())
            .map(str::to_string)
    };
    let ((root, own), subkeys) = keys.split_first()?;
    let filtered = value(own, "UseFilter").is_some_and(|flag| {
        let flag = flag.to_ascii_lowercase();
        flag.strip_prefix("0x")
            .map_or_else(
                || flag.parse::<u64>().ok(),
                |hex| u64::from_str_radix(hex, 16).ok(),
            )
            .is_some_and(|flag| flag != 0)
    });
    if filtered {
        let normal = |path: &str| {
            path.trim()
                .trim_matches('"')
                .replace('/', "\\")
                .to_lowercase()
        };
        let wanted = normal(&program.to_string_lossy());
        let direct = |key: &str| {
            key.get(root.len()..)
                .and_then(|rest| rest.strip_prefix('\\'))
                .is_some_and(|name| !name.is_empty() && !name.contains('\\'))
                && key[..root.len()].eq_ignore_ascii_case(root)
        };
        let matched = subkeys
            .iter()
            .filter(|(key, _)| direct(key))
            .filter(|(_, values)| {
                value(values, "FilterFullPath").is_some_and(|p| normal(&p) == wanted)
            })
            .find_map(|(_, values)| value(values, "Debugger"));
        if matched.is_some() {
            return matched;
        }
    }
    value(own, "Debugger")
}
/// One `    Name    REG_TYPE    Data` line of `reg query` output.
#[cfg_attr(not(windows), allow(dead_code))]
fn reg_value(line: &str) -> Option<(&str, &str)> {
    if !line.starts_with("    ") {
        return None;
    }
    let line = line.trim_start();
    let at = line.find("    REG_")?;
    let (name, rest) = (&line[..at], &line[at + 4..]);
    let data = rest.find("    ").map_or("", |end| &rest[end..]);
    Some((name, data.trim()))
}
/// The `(Default)` value of an App Paths key as `reg query <key> /ve` prints
/// it: optionally quoted, `REG_EXPAND_SZ` values with `%VARIABLE%`s expanded.
#[cfg_attr(not(windows), allow(dead_code))]
fn app_paths_default(output: &str, variable: impl Fn(&str) -> Option<String>) -> Option<PathBuf> {
    let line = output
        .lines()
        .find(|line| line.contains("REG_SZ") || line.contains("REG_EXPAND_SZ"))?;
    let (kind, value) = if let Some((_, value)) = line.split_once("REG_EXPAND_SZ") {
        (true, value)
    } else {
        (false, line.split_once("REG_SZ")?.1)
    };
    let value = value.trim().trim_matches('"').trim();
    if value.is_empty() {
        return None;
    }
    if !kind {
        return Some(PathBuf::from(value));
    }
    let mut expanded = String::new();
    let mut rest = value;
    while let Some(start) = rest.find('%') {
        expanded.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        match after.find('%') {
            Some(end) if end > 0 => {
                let name = &after[..end];
                match variable(name) {
                    Some(text) => expanded.push_str(&text),
                    None => {
                        expanded.push('%');
                        expanded.push_str(name);
                        expanded.push('%');
                    }
                }
                rest = &after[end + 1..];
            }
            _ => {
                expanded.push('%');
                rest = after;
            }
        }
    }
    expanded.push_str(rest);
    Some(PathBuf::from(expanded))
}
pub fn file_url(path: &std::path::Path) -> Result<String, String> {
    url::Url::from_file_path(path)
        .map(String::from)
        .map_err(|_| "Use an absolute file path".into())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn urls_cannot_inject_browser_options_or_shell_arguments() {
        let p = Profile {
            program: "browser".into(),
            arguments: "--new-window $URL$".into(),
            ..Default::default()
        };
        let Launch::Program { args, .. } =
            launch(&p, "https://example.org/?q=hello%20world&next=--private").unwrap()
        else {
            panic!()
        };
        assert_eq!(args.len(), 2);
        assert_eq!(args[0], "--new-window");
        assert!(args[1].contains("&next=--private"));
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn known_browser_kinds_resolve_vendor_channel_names_on_path() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let stable = dir.path().join("microsoft-edge-stable");
        std::fs::write(&stable, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&stable, std::fs::Permissions::from_mode(0o755)).unwrap();
        let found = |profile: &Profile| detect_program_in(profile, dir.path().as_os_str());
        let edge = Profile::template(Kind::Edge);
        assert_eq!(found(&edge), Some(stable.clone()), "channel package name");
        let custom = Profile {
            program: "microsoft-edge-stable".into(),
            ..Default::default()
        };
        assert_eq!(
            found(&custom),
            Some(stable),
            "custom bare names resolve on PATH"
        );
        let firefox = Profile::template(Kind::Firefox);
        assert_eq!(found(&firefox), None, "absent kinds are not invented");
        let absolute = Profile {
            program: "/opt/does/not/exist".into(),
            ..Profile::template(Kind::Edge)
        };
        assert_eq!(found(&absolute), None, "explicit paths are never rewritten");
    }
    #[test]
    fn windows_app_paths_default_values_are_parsed_and_expanded() {
        let plain = "\r\nHKEY_LOCAL_MACHINE\\Software\\Microsoft\\Windows\\CurrentVersion\\App Paths\\chrome.exe\r\n    (Default)    REG_SZ    C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe\r\n\r\n";
        assert_eq!(
            app_paths_default(plain, |_| None),
            Some(PathBuf::from(
                "C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe"
            ))
        );
        let quoted =
            "    (Default)    REG_SZ    \"C:\\Program Files\\Mozilla Firefox\\firefox.exe\"\r\n";
        assert_eq!(
            app_paths_default(quoted, |_| None),
            Some(PathBuf::from(
                "C:\\Program Files\\Mozilla Firefox\\firefox.exe"
            ))
        );
        let expand =
            "    (Default)    REG_EXPAND_SZ    %LOCALAPPDATA%\\Programs\\Browser\\b.exe\r\n";
        let lookup = |name: &str| {
            (name == "LOCALAPPDATA").then(|| "C:\\Users\\Ann Lee\\AppData\\Local".to_string())
        };
        assert_eq!(
            app_paths_default(expand, lookup),
            Some(PathBuf::from(
                "C:\\Users\\Ann Lee\\AppData\\Local\\Programs\\Browser\\b.exe"
            ))
        );
        let unknown = "    (Default)    REG_EXPAND_SZ    %NOPE%\\b.exe 100%\r\n";
        assert_eq!(
            app_paths_default(unknown, |_| None),
            Some(PathBuf::from("%NOPE%\\b.exe 100%"))
        );
        assert_eq!(
            app_paths_default(
                "ERROR: The system was unable to find the specified registry key or value.",
                |_| None
            ),
            None
        );
        assert_eq!(
            app_paths_default("    (Default)    REG_SZ    \r\n", |_| None),
            None
        );
    }
    #[test]
    fn windows_ifeo_debugger_redirects_are_detected() {
        let edge = Path::new("C:\\Program Files (x86)\\Microsoft\\Edge\\Application\\msedge.exe");
        let key = "HKEY_LOCAL_MACHINE\\SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion\\Image File Execution Options\\msedge.exe";
        let plain = format!(
            "\r\n{key}\r\n    Debugger    REG_SZ    \"C:\\Program Files\\MSEdgeRedirect\\MSEdgeRedirect.exe\"\r\n\r\n"
        );
        let debugger = ifeo_debugger(&plain, edge).unwrap();
        assert_eq!(
            debugger,
            "\"C:\\Program Files\\MSEdgeRedirect\\MSEdgeRedirect.exe\""
        );
        assert_eq!(debugger_name(&debugger), "MSEdgeRedirect.exe");
        assert_eq!(
            redirect_message("Edge", &debugger),
            "Edge launches are redirected by MSEdgeRedirect.exe (Image File Execution \
             Options); RustPy cannot open pages in Edge on this machine."
        );
        // MSEdgeRedirect's filtered registration: the image key only sets
        // UseFilter; a subkey names the exact Edge executable.
        let filtered = format!(
            "\r\n{key}\r\n    UseFilter    REG_DWORD    0x1\r\n\r\n\
             {key}\\0\r\n    Debugger    REG_SZ    C:\\Program Files\\MSEdgeRedirect\\MSEdgeRedirect.exe --ifeo\r\n    \
             FilterFullPath    REG_SZ    c:\\program files (x86)\\microsoft\\edge\\application\\MSEDGE.EXE\r\n\r\n\
             {key}\\MSER\r\n    Debugger    REG_SZ    C:\\Other\\other.exe\r\n    \
             FilterFullPath    REG_SZ    C:\\Elsewhere\\msedge.exe\r\n\r\n"
        );
        let debugger = ifeo_debugger(&filtered, edge).unwrap();
        assert_eq!(
            debugger,
            "C:\\Program Files\\MSEdgeRedirect\\MSEdgeRedirect.exe --ifeo"
        );
        assert_eq!(debugger_name(&debugger), "MSEdgeRedirect.exe");
        assert_eq!(
            ifeo_debugger(&filtered, Path::new("C:\\Elsewhere\\msedge.exe")).as_deref(),
            Some("C:\\Other\\other.exe")
        );
        assert_eq!(
            ifeo_debugger(&filtered, Path::new("D:\\Portable\\msedge.exe")),
            None,
            "a filter for another path does not apply"
        );
        // Filters are ignored without UseFilter; nested keys are not filters.
        let unfiltered = filtered.replace("0x1", "0x0");
        assert_eq!(ifeo_debugger(&unfiltered, edge), None);
        let nested = format!(
            "{key}\r\n    UseFilter    REG_DWORD    0x1\r\n\r\n{key}\\0\\deeper\r\n    \
             Debugger    REG_SZ    x.exe\r\n    FilterFullPath    REG_SZ    {}\r\n",
            edge.display()
        );
        assert_eq!(ifeo_debugger(&nested, edge), None);
        // Image keys with unrelated options, empty values or no key at all.
        let options = format!(
            "{key}\r\n    MitigationOptions    REG_BINARY    0001\r\n    Debugger    REG_SZ    \r\n"
        );
        assert_eq!(ifeo_debugger(&options, edge), None);
        assert_eq!(
            ifeo_debugger(
                "ERROR: The system was unable to find the specified registry key or value.\r\n",
                edge
            ),
            None
        );
        assert_eq!(debugger_name("C:\\Tools\\redirect"), "redirect");
    }
    #[test]
    fn active_and_credential_urls_are_rejected() {
        for url in [
            "javascript:alert(1)",
            "data:text/html,x",
            "https://user:secret@example.org/",
            "https://example.org/\n--flag",
        ] {
            assert!(validated_url(url).is_err());
        }
    }
}

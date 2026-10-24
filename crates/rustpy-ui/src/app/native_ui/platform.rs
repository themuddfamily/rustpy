//! System dialog commands, invoked only by a retained background task.
use rustpy_core::CancellationToken;
#[cfg(any(target_os = "linux", test))]
use rustpy_settings::native_ui::LinuxProvider;
use rustpy_settings::native_ui::Settings;
use std::{
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Kind {
    File,
    Directory,
    Save,
}
#[cfg(any(target_os = "macos", target_os = "windows", test))]
impl Kind {
    fn mode(self) -> &'static str {
        match self {
            Self::File => "file",
            Self::Directory => "directory",
            Self::Save => "save",
        }
    }
}
#[derive(Debug)]
struct Plan {
    program: PathBuf,
    args: Vec<String>,
    request: Option<String>,
    json: bool,
}
#[cfg(any(target_os = "linux", test))]
fn linux_plan(provider: LinuxProvider, kind: Kind, directory: &Path, name: &str) -> Plan {
    linux_plan_with_program(
        provider,
        kind,
        directory,
        name,
        if provider == LinuxProvider::Kdialog {
            "/usr/bin/kdialog"
        } else {
            "/usr/bin/zenity"
        },
    )
}
#[cfg(any(target_os = "linux", test))]
fn linux_plan_with_program(
    provider: LinuxProvider,
    kind: Kind,
    directory: &Path,
    name: &str,
    program: &str,
) -> Plan {
    let mut args = vec![
        "-c".into(),
        include_str!("chooser.sh").into(),
        "rustpy-native-dialog".into(),
    ];
    let path = if kind == Kind::Save {
        directory.join(name)
    } else {
        directory.to_path_buf()
    };
    if provider == LinuxProvider::Kdialog {
        args.extend([
            program.into(),
            match kind {
                Kind::File => "--getopenfilename",
                Kind::Directory => "--getexistingdirectory",
                Kind::Save => "--getsavefilename",
            }
            .into(),
            path.to_string_lossy().into_owned(),
        ]);
    } else {
        args.extend([
            program.into(),
            "--file-selection".into(),
            "--title=RustPy".into(),
        ]);
        let mut initial = path.to_string_lossy().into_owned();
        if kind != Kind::Save && !initial.ends_with('/') {
            initial.push('/');
        }
        args.push(format!("--filename={initial}"));
        match kind {
            Kind::Directory => args.push("--directory".into()),
            Kind::Save => args.extend(["--save".into(), "--confirm-overwrite".into()]),
            Kind::File => {}
        }
    }
    Plan {
        program: "/bin/sh".into(),
        args,
        request: None,
        json: false,
    }
}
#[cfg(any(target_os = "linux", all(test, unix)))]
fn checked_provider(path: &Path) -> Result<String, String> {
    let text = path
        .to_str()
        .ok_or("The configured native dialog executable must use a UTF-8 path")?;
    if !path.is_absolute() || text.chars().any(char::is_control) {
        return Err(
            "Choose an absolute native dialog executable path without control characters".into(),
        );
    }
    let canonical = path
        .canonicalize()
        .map_err(|_| "The configured native dialog executable is unavailable")?;
    let metadata = canonical
        .metadata()
        .map_err(|_| "The configured native dialog executable is unavailable")?;
    if !metadata.is_file() {
        return Err("The configured native dialog executable must be a regular file".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o111 == 0 {
            return Err("The configured native dialog file is not executable".into());
        }
    }
    let text = canonical
        .to_str()
        .ok_or("The native dialog executable resolves to a non-UTF-8 path")?;
    if text.chars().any(char::is_control) {
        return Err(
            "The native dialog executable resolves to a path with control characters".into(),
        );
    }
    Ok(text.into())
}
#[cfg(any(target_os = "macos", test))]
fn macos_plan(kind: Kind, directory: &Path, name: &str) -> Plan {
    Plan {
        program: "/usr/bin/osascript".into(),
        args: vec![
            "-e".into(),
            include_str!("chooser.applescript").into(),
            "--".into(),
            kind.mode().into(),
            directory.to_string_lossy().into_owned(),
            name.into(),
        ],
        request: None,
        json: false,
    }
}
#[cfg(any(target_os = "windows", test))]
fn windows_plan(kind: Kind, directory: &Path, name: &str) -> Plan {
    let system = std::env::var_os("SystemRoot")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
    Plan {
        program: system.join(r"System32\WindowsPowerShell\v1.0\powershell.exe"),
        args: vec![
            "-NoLogo".into(),
            "-NoProfile".into(),
            "-NonInteractive".into(),
            "-STA".into(),
            "-Command".into(),
            include_str!("chooser.ps1").into(),
        ],
        request: Some(
            serde_json::json!({"mode":kind.mode(),"directory":directory,"name":name}).to_string(),
        ),
        json: true,
    }
}
fn decode(stdout: &[u8], json: bool) -> Result<Option<PathBuf>, String> {
    let path = if json {
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Chosen {
            path: Option<PathBuf>,
        }
        serde_json::from_slice::<Chosen>(stdout)
            .map_err(|e| format!("Invalid native chooser response: {e}"))?
            .path
    } else {
        let text = std::str::from_utf8(stdout)
            .map_err(|_| "The native chooser returned a non-UTF-8 path")?;
        let text = text.strip_suffix('\n').unwrap_or(text);
        let text = text.strip_suffix('\r').unwrap_or(text);
        if text == "RUSTPY_NATIVE_CANCELLED" {
            None
        } else {
            Some(PathBuf::from(text))
        }
    };
    if path.as_ref().is_some_and(|path| {
        !path.is_absolute() || path.as_os_str().is_empty() || path.to_string_lossy().contains('\0')
    }) {
        return Err("Native chooser did not return an absolute path".into());
    }
    Ok(path)
}
fn execute(plan: Plan, cancel: &CancellationToken) -> Result<Option<PathBuf>, String> {
    cancel.check().map_err(|e| e.to_string())?;
    let mut command = Command::new(plan.program);
    command.args(plan.args);
    // No project working directory or project-defined command is involved.
    command.current_dir(std::env::temp_dir());
    if let Some(request) = plan.request {
        command.env("RUSTPY_NATIVE_CHOOSER", request);
    }
    let (stdout, _) = rustpy_project::native_skeleton::supervised_tool(
        &mut command,
        cancel,
        Duration::from_secs(900),
        65536,
    )
    .map_err(|e| e.to_string())?;
    cancel.check().map_err(|e| e.to_string())?;
    decode(&stdout, plan.json)
}
pub(super) fn choose(
    #[cfg(target_os = "linux")] settings: &Settings,
    #[cfg(not(target_os = "linux"))] _settings: &Settings,
    #[cfg(any(target_os = "windows", target_os = "macos", target_os = "linux"))] kind: Kind,
    #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))] _kind: Kind,
    directory: &Path,
    #[cfg(any(target_os = "windows", target_os = "macos", target_os = "linux"))] name: &str,
    #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))] _name: &str,
    #[cfg(any(target_os = "windows", target_os = "macos", target_os = "linux"))]
    cancel: &CancellationToken,
    #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
    _cancel: &CancellationToken,
) -> Result<Option<PathBuf>, String> {
    if directory.to_str().is_none() {
        return Err(
            "This native provider cannot represent the initial folder; use the built-in browser"
                .into(),
        );
    }
    if !directory.is_absolute() || !directory.is_dir() {
        return Err("The initial chooser folder is unavailable".into());
    }
    #[cfg(target_os = "windows")]
    {
        execute(windows_plan(kind, directory, name), cancel)
    }
    #[cfg(target_os = "macos")]
    {
        execute(macos_plan(kind, directory, name), cancel)
    }
    #[cfg(target_os = "linux")]
    {
        let providers = match settings.linux_provider {
            LinuxProvider::Auto => {
                if std::env::var("XDG_CURRENT_DESKTOP")
                    .unwrap_or_default()
                    .to_ascii_lowercase()
                    .contains("kde")
                {
                    vec![LinuxProvider::Kdialog, LinuxProvider::Zenity]
                } else {
                    vec![LinuxProvider::Zenity, LinuxProvider::Kdialog]
                }
            }
            provider => vec![provider],
        };
        let mut errors = Vec::new();
        for provider in providers {
            let selected = match provider {
                LinuxProvider::Kdialog => settings.kdialog_program.as_deref(),
                _ => settings.zenity_program.as_deref(),
            };
            let plan = if let Some(selected) = selected {
                let program = checked_provider(selected)?;
                linux_plan_with_program(provider, kind, directory, name, &program)
            } else {
                linux_plan(provider, kind, directory, name)
            };
            match execute(plan, cancel) {
                Ok(result) => return Ok(result),
                Err(error) => errors.push(error),
            }
            cancel.check().map_err(|e| e.to_string())?;
        }
        Err(format!("Native chooser unavailable: {}", errors.join("; ")))
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
    {
        Err("Native file panels are unavailable on this platform".into())
    }
}
#[cfg(test)]
mod fixtures {
    use super::*;
    #[test]
    fn provider_commands_keep_paths_out_of_script_source() {
        let path = Path::new("/tmp/quotes ' ; $(touch BAD)\nfolder");
        for kind in [Kind::File, Kind::Directory, Kind::Save] {
            let mac = macos_plan(kind, path, "name ' x.txt");
            assert_eq!(mac.args[4], path.to_string_lossy());
            assert!(!mac.args[1].contains("touch BAD"));
            let linux = linux_plan(LinuxProvider::Zenity, kind, path, "file.txt");
            assert!(!linux.args[1].contains("touch BAD"));
            assert!(linux.args.iter().any(|arg| arg.contains("touch BAD")));
            let windows = windows_plan(kind, path, "file.txt");
            assert!(!windows.args.last().unwrap().contains("touch BAD"));
            assert!(windows.request.unwrap().contains("touch BAD"));
        }
    }
    #[test]
    fn cancellation_and_path_decoding_preserve_spaces_and_reject_unowned_output() {
        assert_eq!(decode(b"RUSTPY_NATIVE_CANCELLED\n", false).unwrap(), None);
        assert_eq!(decode(br#"{"path":null}"#, true).unwrap(), None);
        let absolute = if cfg!(windows) {
            r"C:\spaces "
        } else {
            "/tmp/ spaces "
        };
        assert_eq!(
            decode(format!("{absolute}\n").as_bytes(), false).unwrap(),
            Some(PathBuf::from(absolute))
        );
        assert!(decode(b"relative-path\n", false).is_err());
        assert!(decode(b"", false).is_err());
        assert!(decode(br#"{"path":null,"execute":"python"}"#, true).is_err());
    }
    #[cfg(unix)]
    #[test]
    fn explicit_provider_is_validated_and_stays_one_positional_argument() {
        use std::os::unix::fs::PermissionsExt;
        let folder = tempfile::tempdir().unwrap();
        let provider = folder.path().join("real provider ' λ");
        std::fs::write(&provider, b"fixture bytes, never executed").unwrap();
        std::fs::set_permissions(&provider, std::fs::Permissions::from_mode(0o700)).unwrap();
        let selected = checked_provider(&provider).unwrap();
        let plan = linux_plan_with_program(
            LinuxProvider::Zenity,
            Kind::File,
            folder.path(),
            "",
            &selected,
        );
        assert_eq!(plan.program, PathBuf::from("/bin/sh"));
        assert_eq!(plan.args[3], selected);
        assert!(!plan.args[1].contains("real provider"));
        assert_eq!(plan.args[4], "--file-selection");
        assert_eq!(
            plan.args.last().unwrap(),
            &format!("--filename={}/", folder.path().display())
        );
        assert_eq!(
            linux_plan(LinuxProvider::Zenity, Kind::File, folder.path(), "").args[3],
            "/usr/bin/zenity"
        );
        assert_eq!(
            linux_plan(LinuxProvider::Kdialog, Kind::File, folder.path(), "").args[3],
            "/usr/bin/kdialog"
        );
        assert!(checked_provider(Path::new("project/zenity")).is_err());
        assert!(checked_provider(&folder.path().join("missing")).is_err());
        assert!(checked_provider(folder.path()).is_err());
        assert!(checked_provider(Path::new("/tmp/provider\nname")).is_err());
        std::fs::set_permissions(&provider, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(checked_provider(&provider).is_err());
    }
}

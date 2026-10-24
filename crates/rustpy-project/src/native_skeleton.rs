//! Explicit trusted-library native skeleton producer; run only in a managed task.
use anyhow::{ensure, Context, Result};
use rustpy_core::CancellationToken;
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

/// Run a trusted tool on a background worker with bounded output, cancellation,
/// and owned process-tree kill and leader reap on timeout. The caller never
/// inherits stdin.
pub fn supervised_tool(
    command: &mut Command,
    cancel: &CancellationToken,
    timeout: Duration,
    output_limit: u64,
) -> Result<(Vec<u8>, Vec<u8>)> {
    portable_child::supervised(command, cancel, timeout, output_limit)
}

const MAX_OUTPUT: u64 = 16 * 1024 * 1024;
const MAX_STUB: usize = 2 * 1024 * 1024;
const DEADLINE: Duration = Duration::from_secs(15);
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Request {
    pub interpreter: PathBuf,
    pub interpreter_sha256: String,
    pub module_name: String,
    pub module_path: PathBuf,
    pub module_sha256: String,
    pub library_root: PathBuf,
    pub expected_version: String,
    pub expected_cache_tag: String,
    pub expected_soabi: String,
    pub explicitly_trusted: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Artifact {
    pub protocol: u32,
    pub interpreter: PathBuf,
    pub interpreter_sha256: String,
    pub version: String,
    pub cache_tag: String,
    pub soabi: String,
    pub module_name: String,
    pub module_path: PathBuf,
    pub module_sha256: String,
    pub library_root: PathBuf,
    pub pyi: String,
    pub unsupported: Vec<String>,
}
#[path = "portable_child.rs"]
mod portable_child;
use portable_child::supervised;
fn hex_sha(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
/// Worker-only hashing; never call from rendering/shortcut handlers.
pub fn fingerprint_file(path: &Path, cancel: &CancellationToken) -> Result<String> {
    use sha2::{Digest, Sha256};
    use std::fs::OpenOptions;
    cancel.check()?;
    ensure!(
        fs::canonicalize(path)? == path,
        "noncanonical fingerprint path"
    );
    let before = fs::symlink_metadata(path)?;
    ensure!(
        before.is_file() && before.len() <= 256 * 1024 * 1024,
        "binary fingerprint budget/type"
    );
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.custom_flags(0x0020_0000);
    }
    let mut file = options.open(path)?;
    let mut hash = Sha256::new();
    let mut buffer = [0; 65536];
    let mut count = 0;
    loop {
        cancel.check()?;
        let size = file.read(&mut buffer)?;
        if size == 0 {
            break;
        }
        count += size as u64;
        ensure!(count <= 256 * 1024 * 1024, "binary fingerprint growth");
        hash.update(&buffer[..size]);
    }
    let after = fs::symlink_metadata(path)?;
    let descriptor = file.metadata()?;
    ensure!(
        after.is_file()
            && count == before.len()
            && after.len() == before.len()
            && descriptor.len() == before.len()
            && after.modified()? == before.modified()?
            && descriptor.modified()? == before.modified()?,
        "binary changed while hashing"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        ensure!(
            (
                before.dev(),
                before.ino(),
                before.ctime(),
                before.ctime_nsec()
            ) == (after.dev(), after.ino(), after.ctime(), after.ctime_nsec())
                && (before.dev(), before.ino()) == (descriptor.dev(), descriptor.ino()),
            "binary identity changed"
        );
    }
    cancel.check()?;
    Ok(format!("{:x}", hash.finalize()))
}
/// Managed-worker entry: all expensive preparation happens off the UI thread.
pub fn prepare_and_produce(
    interpreter: &Path,
    selected_version: &str,
    module_path: &Path,
    module_name: &str,
    library_root: &Path,
    cancel: &CancellationToken,
) -> Result<Artifact> {
    let interpreter_sha256 = fingerprint_file(interpreter, cancel)?;
    let module_sha256 = fingerprint_file(module_path, cancel)?;
    let mut command = crate::interpreter::interpreter_command(interpreter);
    command.args(["-I", "-S", "-c", "import json,sys,sysconfig;print(json.dumps({'version':'.'.join(map(str,sys.version_info[:3])),'cache_tag':sys.implementation.cache_tag,'soabi':sysconfig.get_config_var('SOABI') or ''}))"]).env_clear().current_dir(library_root);
    let (output, _) = supervised(&mut command, cancel, DEADLINE, MAX_OUTPUT)?;
    #[derive(Deserialize)]
    struct Probe {
        version: String,
        cache_tag: String,
        soabi: String,
    }
    let probe: Probe = serde_json::from_slice(&output)?;
    ensure!(
        probe.version == selected_version,
        "selected interpreter version changed"
    );
    ensure!(
        fingerprint_file(interpreter, cancel)? == interpreter_sha256
            && fingerprint_file(module_path, cancel)? == module_sha256,
        "binary changed during preparation"
    );
    let request = Request {
        interpreter: interpreter.to_path_buf(),
        interpreter_sha256,
        module_name: module_name.to_owned(),
        module_path: module_path.to_path_buf(),
        module_sha256,
        library_root: library_root.to_path_buf(),
        expected_version: probe.version,
        expected_cache_tag: probe.cache_tag,
        expected_soabi: probe.soabi,
        explicitly_trusted: true,
    };
    let cache = skeleton_cache_dir();
    let path = cache.join(format!(
        "{}-{}.json",
        request.interpreter_sha256, request.module_sha256
    ));
    if let Ok(bytes) = (|| -> std::io::Result<Vec<u8>> {
        let mut bytes = Vec::new();
        fs::File::open(&path)?
            .take((MAX_STUB + 1024 * 1024 + 1) as u64)
            .read_to_end(&mut bytes)?;
        Ok(bytes)
    })() {
        if bytes.len() <= MAX_STUB + 1024 * 1024 {
            if let Ok(artifact) = serde_json::from_slice::<Artifact>(&bytes) {
                if artifact.protocol == 1
                    && artifact.interpreter == request.interpreter
                    && artifact.interpreter_sha256 == request.interpreter_sha256
                    && artifact.version == request.expected_version
                    && artifact.cache_tag == request.expected_cache_tag
                    && artifact.soabi == request.expected_soabi
                    && artifact.module_path == request.module_path
                    && artifact.module_name == request.module_name
                    && artifact.module_sha256 == request.module_sha256
                    && artifact.library_root == request.library_root
                    && artifact.pyi.len() <= MAX_STUB
                    && artifact.unsupported.len() <= 2048
                {
                    cancel.check()?;
                    return Ok(artifact);
                }
            }
        }
    }
    let artifact = produce(&request, cancel)?;
    cancel.check()?;
    if fs::create_dir_all(&cache).is_ok() {
        if let Ok(bytes) = serde_json::to_vec(&artifact) {
            let _ = fs::write(path, bytes);
        }
    }
    Ok(artifact)
}
fn skeleton_cache_dir() -> PathBuf {
    rustpy_settings::Settings::system_dir().join("native-skeleton-cache")
}
/// Rebuild invalidates only this interpreter's generated artifacts.
pub fn clear_interpreter_cache(interpreter: &Path, cancel: &CancellationToken) -> Result<()> {
    let hash = fingerprint_file(interpreter, cancel)?;
    let Ok(entries) = fs::read_dir(skeleton_cache_dir()) else {
        return Ok(());
    };
    for entry in entries.take(8192) {
        cancel.check()?;
        let entry = entry?;
        if entry.file_type()?.is_file()
            && entry
                .file_name()
                .to_string_lossy()
                .starts_with(&format!("{hash}-"))
        {
            fs::remove_file(entry.path())?;
        }
    }
    Ok(())
}

pub fn produce(request: &Request, cancel: &CancellationToken) -> Result<Artifact> {
    ensure!(
        request.explicitly_trusted,
        "explicit trusted-library approval required"
    );
    ensure!(
        hex_sha(&request.module_sha256) && hex_sha(&request.interpreter_sha256),
        "invalid content binding"
    );
    ensure!(
        !request.module_name.is_empty()
            && request.module_name.split('.').all(|part| !part.is_empty()),
        "qualified module name required"
    );
    ensure!(
        fs::canonicalize(&request.interpreter)? == request.interpreter,
        "noncanonical interpreter"
    );
    ensure!(
        fs::canonicalize(&request.module_path)? == request.module_path,
        "noncanonical module"
    );
    ensure!(
        fs::canonicalize(&request.library_root)? == request.library_root
            && request.module_path.starts_with(&request.library_root),
        "module outside canonical admitted library root"
    );
    cancel.check()?;
    let mut command = crate::interpreter::interpreter_command(&request.interpreter);
    command
        .args(["-I", "-S", "-c", include_str!("introspect.py")])
        .arg(serde_json::to_string(request)?)
        .current_dir(&request.library_root)
        .env_clear();
    let (stdout, stderr) = supervised(&mut command, cancel, DEADLINE, MAX_OUTPUT)?;
    ensure!(stderr.len() <= MAX_OUTPUT as usize, "stderr limit");
    let artifact: Artifact =
        serde_json::from_slice(&stdout).context("invalid native skeleton response")?;
    ensure!(
        artifact.protocol == 1
            && artifact.module_name == request.module_name
            && artifact.module_path == request.module_path
            && artifact.module_sha256 == request.module_sha256
            && artifact.library_root == request.library_root,
        "module binding mismatch"
    );
    ensure!(
        artifact.interpreter == request.interpreter
            && artifact.interpreter_sha256 == request.interpreter_sha256
            && artifact.version == request.expected_version
            && artifact.cache_tag == request.expected_cache_tag
            && artifact.soabi == request.expected_soabi,
        "interpreter binding mismatch"
    );
    ensure!(
        artifact.pyi.len() <= MAX_STUB && artifact.unsupported.len() <= 2048,
        "artifact budget"
    );
    ensure!(
        fingerprint_file(&request.interpreter, cancel)? == request.interpreter_sha256
            && fingerprint_file(&request.module_path, cancel)? == request.module_sha256,
        "native bindings changed after introspection"
    );
    cancel.check()?;
    Ok(artifact)
}
#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::time::Instant;
    fn child(script: &str) -> (tempfile::TempDir, Command, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("child");
        fs::write(&path, format!("#!/bin/sh\n{script}\n")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        let pid = dir.path().join("pid");
        let mut command = Command::new(path);
        command.arg(&pid);
        (dir, command, pid)
    }
    fn absent(pid: &PathBuf) {
        let id = fs::read_to_string(pid).unwrap();
        assert!(!PathBuf::from(format!("/proc/{}", id.trim())).exists());
    }
    #[test]
    fn owned_timeout_kills_and_reaps_child() {
        let (_dir, mut cmd, pid) = child("echo $$ > \"$1\"; while :; do :; done");
        let mut owned = None;
        let result = portable_child::supervised_with_owned_pid(
            &mut cmd,
            &CancellationToken::new(),
            Duration::from_millis(100),
            MAX_OUTPUT,
            &mut owned,
        );
        assert!(result.is_err());
        let error = result.unwrap_err();
        assert!(error.to_string().contains("timeout"), "{error:#}");
        let owned = owned.expect("fixture must spawn the actual owned child");
        // The OS-owned identity exists before the script can publish its PID.
        // Timeout may retire that process before its first echo finishes.
        match fs::read_to_string(&pid) {
            Ok(published) => assert_eq!(published.trim().parse::<u32>().unwrap(), owned),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => panic!("unexpected PID observation failure: {error}"),
        }
        assert!(!PathBuf::from(format!("/proc/{owned}")).exists());
    }
    #[test]
    fn timeout_before_pid_publication_still_proves_exact_owned_child_retirement() {
        // This child deliberately never publishes a PID file. Observation must
        // therefore prove OS ownership and retirement without script cooperation.
        let (_dir, mut cmd, pid) = child("while :; do :; done");
        let mut owned = None;
        let result = portable_child::supervised_with_owned_pid(
            &mut cmd,
            &CancellationToken::new(),
            Duration::from_millis(100),
            MAX_OUTPUT,
            &mut owned,
        );
        let error = result.unwrap_err();
        assert!(error.to_string().contains("timeout"), "{error:#}");
        let owned = owned.expect("prepublication fixture must spawn an actual child");
        assert_eq!(
            fs::read_to_string(&pid).unwrap_err().kind(),
            std::io::ErrorKind::NotFound
        );
        assert!(!PathBuf::from(format!("/proc/{owned}")).exists());
    }

    #[test]
    fn output_limit_kills_and_reaps_child() {
        let (_dir, mut cmd, pid) = child(
            "echo $$ > \"$1\"; while :; do printf '0123456789012345678901234567890123456789012345678901234567890123456789'; done",
        );
        let error = supervised(
            &mut cmd,
            &CancellationToken::new(),
            Duration::from_secs(5),
            1024,
        )
        .unwrap_err();
        assert!(error.to_string().contains("output limit"));
        absent(&pid);
    }
    #[test]
    fn cancellation_after_real_owned_start_reaps_child() {
        let (_dir, mut cmd, pid) = child("echo $$ > \"$1\"; while :; do :; done");
        let token = CancellationToken::new();
        let worker_token = token.clone();
        let worker = std::thread::spawn(move || {
            supervised(&mut cmd, &worker_token, Duration::from_secs(2), MAX_OUTPUT)
        });
        let start = Instant::now();
        while fs::read_to_string(&pid)
            .ok()
            .and_then(|s| s.trim().parse::<u32>().ok())
            .is_none()
        {
            assert!(start.elapsed() < Duration::from_secs(1));
            std::thread::yield_now();
        }
        token.cancel();
        assert!(worker.join().unwrap().is_err());
        absent(&pid);
    }
    #[test]
    fn closed_pipe_success_is_reaped_and_exact_output_retained() {
        let (_dir, mut cmd, pid) =
            child("echo $$ > \"$1\"; if read x; then exit 9; fi; printf '{\"ok\":true}'");
        let (out, err) = supervised(
            &mut cmd,
            &CancellationToken::new(),
            Duration::from_secs(2),
            MAX_OUTPUT,
        )
        .unwrap();
        assert_eq!(out, b"{\"ok\":true}");
        assert!(err.is_empty());
        absent(&pid);
    }
}

/// Run an IDE-owned inspection tool using the same portable cancellation,
/// deadline, output-limit and shared process-tree cleanup authority as introspection.
/// Project trust must be admitted by the caller before executing project code.
pub fn supervise_tool(
    command: &mut std::process::Command,
    cancel: &rustpy_core::CancellationToken,
    deadline: std::time::Duration,
    output_limit: u64,
) -> anyhow::Result<(Vec<u8>, Vec<u8>)> {
    portable_child::supervised(command, cancel, deadline, output_limit)
}

/// Status-preserving counterpart for validators whose nonzero exit is a finding.
pub fn supervise_tool_status(
    command: &mut std::process::Command,
    cancel: &rustpy_core::CancellationToken,
    deadline: std::time::Duration,
    output_limit: u64,
) -> anyhow::Result<(std::process::ExitStatus, Vec<u8>, Vec<u8>)> {
    portable_child::supervised_status(command, cancel, deadline, output_limit)
}

#[cfg(all(test, unix))]
#[test]
fn inspection_validator_keeps_nonzero_status_and_exact_output() {
    let mut command = std::process::Command::new("sh");
    command.args(["-c", "printf out; printf err >&2; exit 3"]);
    let (status, out, err) = supervise_tool_status(
        &mut command,
        &rustpy_core::CancellationToken::new(),
        std::time::Duration::from_secs(2),
        1024,
    )
    .unwrap();
    assert_eq!(status.code(), Some(3));
    assert_eq!(out, b"out");
    assert_eq!(err, b"err");
}

#[cfg(test)]
mod documentation_source_tests {
    use super::*;
    #[test]
    fn admitted_real_extension_producer_retains_module_and_routine_documentation() {
        let Ok(python) = which::which("python3") else {
            return;
        };
        let Ok(python) = python.canonicalize() else {
            return;
        };
        let cancel = CancellationToken::new();
        let mut probe = Command::new(&python);
        probe.args(["-I","-S","-c","import json,math,sys;print(json.dumps({'path':getattr(math,'__file__',None),'version':'.'.join(map(str,sys.version_info[:3])),'module':math.__doc__.splitlines()[0],'routine':math.sqrt.__doc__}))"]);
        let (bytes, _) =
            supervised_tool(&mut probe, &cancel, Duration::from_secs(5), 65536).unwrap();
        let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let Some(module_path) = value["path"].as_str() else {
            return;
        };
        let module = fs::canonicalize(module_path).unwrap();
        let artifact = prepare_and_produce(
            &python,
            value["version"].as_str().unwrap(),
            &module,
            "math",
            module.parent().unwrap(),
            &cancel,
        )
        .unwrap();
        assert!(artifact.pyi.starts_with('\'') || artifact.pyi.starts_with('"'));
        assert!(artifact.pyi.contains(value["module"].as_str().unwrap()));
        let function = artifact.pyi.find("def sqrt(").unwrap();
        let body = &artifact.pyi[function..];
        let end = body.find("\ndef ").unwrap_or(body.len());
        assert!(body[..end].contains(value["routine"].as_str().unwrap()));
        assert!(artifact.pyi.len() <= MAX_STUB);
    }
}

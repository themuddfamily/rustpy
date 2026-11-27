//! Windows process start for bounded probes without the standard library's
//! process-creation lock.
//!
//! `std::process::Command::spawn` holds a process-wide lock around
//! `CreateProcessW` so that temporarily inheritable handles do not leak into
//! other children. If `CreateProcessW` itself never returns (as happened while
//! a Store app-execution alias waited for package activation), every later
//! `Command::spawn` in the process waits behind it. Probes therefore call
//! `CreateProcessW` themselves with `PROC_THREAD_ATTRIBUTE_HANDLE_LIST`, so the
//! child inherits exactly its three standard handles and no lock is needed; a
//! stuck call only blocks its own disposable thread.

use crate::probe::ProbeError;
use std::ffi::OsStr;
use std::fs::File;
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::fs::{FileExt, OpenOptionsExt};
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::{CloseHandle, SetHandleInformation, WAIT_OBJECT_0};
use windows_sys::Win32::System::Threading::{
    CreateProcessW, DeleteProcThreadAttributeList, GetExitCodeProcess,
    InitializeProcThreadAttributeList, TerminateProcess, UpdateProcThreadAttribute,
    WaitForSingleObject, EXTENDED_STARTUPINFO_PRESENT, PROCESS_INFORMATION,
    PROC_THREAD_ATTRIBUTE_HANDLE_LIST, STARTF_USESTDHANDLES, STARTUPINFOEXW,
};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const CREATE_UNICODE_ENVIRONMENT: u32 = 0x0000_0400;
const HANDLE_FLAG_INHERIT: u32 = 1;
const STILL_ACTIVE: u32 = 259;

/// Delete-on-close temporary file that receives a probe's stdout.
pub(crate) struct OutputFile(File);

impl OutputFile {
    pub(crate) fn create() -> io::Result<Self> {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        const GENERIC_READ: u32 = 0x8000_0000;
        const GENERIC_WRITE: u32 = 0x4000_0000;
        const DELETE: u32 = 0x0001_0000;
        const FILE_SHARE_ALL: u32 = 0x7;
        const FILE_ATTRIBUTE_TEMPORARY: u32 = 0x100;
        const FILE_FLAG_DELETE_ON_CLOSE: u32 = 0x0400_0000;
        let mut last = None;
        for _ in 0..4 {
            let nonce = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.subsec_nanos())
                .unwrap_or(0);
            let name = format!(
                "rustpy-probe-{}-{}-{nonce}.out",
                std::process::id(),
                COUNTER.fetch_add(1, Ordering::Relaxed)
            );
            match std::fs::OpenOptions::new()
                .access_mode(GENERIC_READ | GENERIC_WRITE | DELETE)
                .create_new(true)
                .share_mode(FILE_SHARE_ALL)
                .attributes(FILE_ATTRIBUTE_TEMPORARY)
                .custom_flags(FILE_FLAG_DELETE_ON_CLOSE)
                .open(std::env::temp_dir().join(name))
            {
                Ok(file) => return Ok(Self(file)),
                Err(error) => last = Some(error),
            }
        }
        Err(last.unwrap_or_else(|| io::Error::other("no temporary probe output file")))
    }

    pub(crate) fn try_clone(&self) -> io::Result<File> {
        self.0.try_clone()
    }

    pub(crate) fn len(&self) -> u64 {
        self.0.metadata().map(|m| m.len()).unwrap_or(0)
    }

    pub(crate) fn read(&self, limit: u64) -> Result<Vec<u8>, ProbeError> {
        let len = self.len();
        if len > limit {
            return Err(ProbeError::OutputLimit);
        }
        let mut bytes = vec![0u8; len as usize];
        let mut filled = 0;
        while filled < bytes.len() {
            match self.0.seek_read(&mut bytes[filled..], filled as u64) {
                Ok(0) => break,
                Ok(n) => filled += n,
                Err(_) => return Err(ProbeError::InvalidOutput),
            }
        }
        bytes.truncate(filled);
        Ok(bytes)
    }
}

/// A started probe: our own `CreateProcessW` child, or a batch file started
/// through the standard library (that starts `cmd.exe`, which cannot block in
/// package activation).
pub(crate) enum ProbeChild {
    Raw { process: OwnedHandle, pid: u32 },
    Std(std::process::Child),
}

impl ProbeChild {
    /// Starts `command` with stdin and stderr on `NUL` and stdout on `stdout`.
    /// Only the program, arguments, environment changes and working directory
    /// of `command` are honoured (not `env_clear` or stdio settings).
    pub(crate) fn spawn(mut command: Command, stdout: File) -> io::Result<Self> {
        let program = resolve_program(command.get_program())?;
        if rustpy_core::interpreter_launch::is_batch_file(&program) {
            command
                .creation_flags(CREATE_NO_WINDOW)
                .stdout(Stdio::from(stdout));
            return command.spawn().map(Self::Std);
        }
        #[cfg(test)]
        record_spawn(&program);
        spawn_raw(&command, &program, &stdout)
    }

    pub(crate) fn id(&self) -> u32 {
        match self {
            Self::Raw { pid, .. } => *pid,
            Self::Std(child) => child.id(),
        }
    }

    /// `Some(success)` once the process has exited.
    pub(crate) fn try_wait(&mut self) -> io::Result<Option<bool>> {
        match self {
            Self::Std(child) => Ok(child.try_wait()?.map(|status| status.success())),
            Self::Raw { process, .. } => {
                // SAFETY: the handle is owned and open.
                if unsafe { WaitForSingleObject(process.as_raw_handle(), 0) } != WAIT_OBJECT_0 {
                    return Ok(None);
                }
                let mut code = STILL_ACTIVE;
                // SAFETY: as above; `code` is a valid out pointer.
                if unsafe { GetExitCodeProcess(process.as_raw_handle(), &mut code) } == 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(Some(code == 0))
            }
        }
    }

    pub(crate) fn kill_and_reap(&mut self) {
        match self {
            Self::Std(child) => {
                let _ = child.kill();
                let _ = child.wait();
            }
            Self::Raw { process, .. } => {
                // SAFETY: the handle is owned and open; terminating an exited
                // process fails harmlessly.
                unsafe {
                    TerminateProcess(process.as_raw_handle(), 1);
                    WaitForSingleObject(process.as_raw_handle(), 5_000);
                }
            }
        }
    }
}

/// Kills the child and its descendants (a remote adapter can own SSH, WSL or
/// Python processes), then reaps the child.
pub(crate) fn kill_tree(child: &mut ProbeChild) {
    if let Ok(mut killer) = Command::new("taskkill.exe")
        .creation_flags(CREATE_NO_WINDOW)
        .args(["/F", "/T", "/PID", &child.id().to_string()])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        let deadline = Instant::now() + Duration::from_secs(1);
        while matches!(killer.try_wait(), Ok(None)) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        let _ = killer.kill();
        let _ = killer.wait();
    }
    child.kill_and_reap();
}

/// Resolves the program like `Command` does for the cases probes use: an
/// absolute path, a path relative to the current directory, or a bare name
/// searched on `PATH`; `.exe` is appended when no extension is given.
fn resolve_program(program: &OsStr) -> io::Result<PathBuf> {
    let path = Path::new(program);
    let candidate = if path.is_absolute() {
        path.to_path_buf()
    } else if path.components().count() > 1 {
        std::env::current_dir()?.join(path)
    } else {
        return which::which(program).map_err(|e| {
            io::Error::new(io::ErrorKind::NotFound, format!("{}: {e}", path.display()))
        });
    };
    if candidate.extension().is_none() && !candidate.exists() {
        let exe = candidate.with_extension("exe");
        if exe.exists() {
            return Ok(exe);
        }
    }
    Ok(candidate)
}

/// Appends one argument quoted for `CommandLineToArgvW` / the MSVC runtime.
pub(crate) fn push_arg(out: &mut Vec<u16>, arg: &OsStr, force_quotes: bool) {
    const QUOTE: u16 = b'"' as u16;
    const BACKSLASH: u16 = b'\\' as u16;
    let units: Vec<u16> = arg.encode_wide().collect();
    let quote = force_quotes
        || units.is_empty()
        || units
            .iter()
            .any(|&c| matches!(c, 0x20 | 0x09 | 0x0A | 0x0B) || c == QUOTE);
    if !quote {
        out.extend(units);
        return;
    }
    out.push(QUOTE);
    let mut backslashes = 0;
    for unit in units {
        if unit == BACKSLASH {
            backslashes += 1;
        } else {
            if unit == QUOTE {
                out.extend(std::iter::repeat_n(BACKSLASH, backslashes + 1));
            }
            backslashes = 0;
        }
        out.push(unit);
    }
    out.extend(std::iter::repeat_n(BACKSLASH, backslashes));
    out.push(QUOTE);
}

/// The full command line for `program` and the command's arguments.
pub(crate) fn command_line(program: &Path, command: &Command) -> io::Result<Vec<u16>> {
    if program.as_os_str().encode_wide().any(|c| c == b'"' as u16) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "program path contains a quote",
        ));
    }
    let mut line = Vec::new();
    push_arg(&mut line, program.as_os_str(), true);
    for arg in command.get_args() {
        line.push(b' ' as u16);
        push_arg(&mut line, arg, false);
    }
    if line.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "argument contains a NUL character",
        ));
    }
    line.push(0);
    Ok(line)
}

/// The inherited environment with the command's changes applied, as a sorted
/// `CREATE_UNICODE_ENVIRONMENT` block; `None` when nothing changes.
fn environment_block(command: &Command) -> Option<Vec<u16>> {
    let changes: Vec<_> = command.get_envs().collect();
    if changes.is_empty() {
        return None;
    }
    let mut vars = std::collections::BTreeMap::new();
    let key = |name: &OsStr| name.to_string_lossy().to_uppercase();
    for (name, value) in std::env::vars_os() {
        vars.insert(key(&name), (name, value));
    }
    for (name, value) in changes {
        match value {
            Some(value) => vars.insert(key(name), (name.to_owned(), value.to_owned())),
            None => vars.remove(&key(name)),
        };
    }
    let mut block = Vec::new();
    for (name, value) in vars.values() {
        block.extend(name.encode_wide());
        block.push(b'=' as u16);
        block.extend(value.encode_wide());
        block.push(0);
    }
    if block.is_empty() {
        block.push(0);
    }
    block.push(0);
    Some(block)
}

fn wide(text: &OsStr) -> Vec<u16> {
    text.encode_wide().chain([0]).collect()
}

/// Marks handles inheritable for the duration of one `CreateProcessW`.
struct Inheritable<'a>(&'a [RawHandle]);

impl<'a> Inheritable<'a> {
    fn new(handles: &'a [RawHandle]) -> io::Result<Self> {
        for &handle in handles {
            // SAFETY: the handles are owned by the caller and open.
            if unsafe { SetHandleInformation(handle, HANDLE_FLAG_INHERIT, HANDLE_FLAG_INHERIT) }
                == 0
            {
                return Err(io::Error::last_os_error());
            }
        }
        Ok(Self(handles))
    }
}

impl Drop for Inheritable<'_> {
    fn drop(&mut self) {
        for &handle in self.0 {
            // SAFETY: as in `new`.
            unsafe { SetHandleInformation(handle, HANDLE_FLAG_INHERIT, 0) };
        }
    }
}

/// An initialised one-entry `PROC_THREAD_ATTRIBUTE_LIST`.
struct AttributeList(Vec<u64>);

impl AttributeList {
    fn with_handle_list(handles: &[RawHandle]) -> io::Result<Self> {
        let mut size = 0usize;
        // SAFETY: a null list with a size pointer only queries the size.
        unsafe { InitializeProcThreadAttributeList(std::ptr::null_mut(), 1, 0, &mut size) };
        let mut list = Self(vec![0u64; size.div_ceil(8).max(1)]);
        // SAFETY: the buffer holds at least `size` bytes, 8-byte aligned.
        if unsafe { InitializeProcThreadAttributeList(list.ptr(), 1, 0, &mut size) } == 0 {
            list.0.clear();
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `handles` outlives the list's use in `CreateProcessW`
        // (both live in `spawn_raw`); the size is the array's byte size.
        if unsafe {
            UpdateProcThreadAttribute(
                list.ptr(),
                0,
                PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
                handles.as_ptr().cast(),
                std::mem::size_of_val(handles),
                std::ptr::null_mut(),
                std::ptr::null(),
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(list)
    }

    fn ptr(&mut self) -> *mut core::ffi::c_void {
        self.0.as_mut_ptr().cast()
    }
}

impl Drop for AttributeList {
    fn drop(&mut self) {
        if !self.0.is_empty() {
            // SAFETY: the list was initialised.
            unsafe { DeleteProcThreadAttributeList(self.ptr()) };
        }
    }
}

fn spawn_raw(command: &Command, program: &Path, stdout: &File) -> io::Result<ProbeChild> {
    let nul = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("NUL")?;
    let application = wide(program.as_os_str());
    let mut line = command_line(program, command)?;
    let environment = environment_block(command);
    let directory = command.get_current_dir().map(|dir| wide(dir.as_os_str()));
    let handles = [nul.as_raw_handle(), stdout.as_raw_handle()];
    let mut attributes = AttributeList::with_handle_list(&handles)?;
    let _inheritable = Inheritable::new(&handles)?;
    let mut startup = STARTUPINFOEXW::default();
    startup.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
    startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
    startup.StartupInfo.hStdInput = nul.as_raw_handle();
    startup.StartupInfo.hStdOutput = stdout.as_raw_handle();
    startup.StartupInfo.hStdError = nul.as_raw_handle();
    startup.lpAttributeList = attributes.ptr();
    let mut information = PROCESS_INFORMATION {
        hProcess: std::ptr::null_mut(),
        hThread: std::ptr::null_mut(),
        dwProcessId: 0,
        dwThreadId: 0,
    };
    // SAFETY: every pointer refers to a live, NUL-terminated buffer or
    // initialised structure for the duration of the call; the handle list
    // restricts inheritance to the two handles above.
    let ok = unsafe {
        CreateProcessW(
            application.as_ptr(),
            line.as_mut_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            1,
            CREATE_NO_WINDOW | CREATE_UNICODE_ENVIRONMENT | EXTENDED_STARTUPINFO_PRESENT,
            environment
                .as_ref()
                .map_or(std::ptr::null(), |block| block.as_ptr().cast()),
            directory
                .as_ref()
                .map_or(std::ptr::null(), |dir| dir.as_ptr()),
            &startup.StartupInfo,
            &mut information,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: CreateProcessW returned two new handles that we now own.
    unsafe { CloseHandle(information.hThread) };
    let process = unsafe { OwnedHandle::from_raw_handle(information.hProcess) };
    Ok(ProbeChild::Raw {
        process,
        pid: information.dwProcessId,
    })
}

#[cfg(test)]
fn spawned_programs() -> &'static std::sync::Mutex<Vec<PathBuf>> {
    static SPAWNED: std::sync::OnceLock<std::sync::Mutex<Vec<PathBuf>>> =
        std::sync::OnceLock::new();
    SPAWNED.get_or_init(Default::default)
}

#[cfg(test)]
fn record_spawn(program: &Path) {
    if let Ok(mut spawned) = spawned_programs().lock() {
        spawned.push(program.to_path_buf());
    }
}

/// Programs started by probes in this test process so far.
#[cfg(test)]
pub(crate) fn recorded_spawns() -> Vec<PathBuf> {
    spawned_programs()
        .lock()
        .map(|spawned| spawned.clone())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(args: &[&str]) -> String {
        let mut command = Command::new("x");
        command.args(args);
        let units = command_line(Path::new(r"C:\Program Files\x.exe"), &command).unwrap();
        String::from_utf16(&units[..units.len() - 1]).unwrap()
    }

    #[test]
    fn command_line_quotes_like_the_msvc_runtime() {
        assert_eq!(line(&[]), r#""C:\Program Files\x.exe""#);
        assert_eq!(
            line(&[
                "-I",
                "-c",
                "print(1)",
                "",
                "a b",
                r#"say "hi""#,
                r"dir\",
                r"a\\b"
            ]),
            r#""C:\Program Files\x.exe" -I -c print(1) "" "a b" "say \"hi\"" dir\ a\\b"#
        );
        assert_eq!(
            line(&[r"C:\a b\"]),
            r#""C:\Program Files\x.exe" "C:\a b\\""#
        );
        assert_eq!(line(&[r#"x\"y"#]), r#""C:\Program Files\x.exe" "x\\\"y""#);
    }

    #[test]
    fn environment_changes_are_merged_case_insensitively() {
        let mut command = Command::new("x");
        command
            .env("Path", "C:\\only")
            .env("RUSTPY_PROBE_ENV_TEST", "1");
        let block = environment_block(&command).unwrap();
        let text = String::from_utf16(&block).unwrap();
        let entries: Vec<&str> = text.split('\0').filter(|e| !e.is_empty()).collect();
        let paths: Vec<_> = entries
            .iter()
            .filter(|e| e.to_uppercase().starts_with("PATH="))
            .collect();
        assert_eq!(paths, [&"Path=C:\\only"]);
        assert!(entries.contains(&"RUSTPY_PROBE_ENV_TEST=1"));
        assert!(environment_block(&Command::new("x")).is_none());
    }

    #[test]
    fn raw_spawn_captures_output_with_arguments_environment_and_directory() {
        let dir = tempfile::tempdir().unwrap();
        let mut command = Command::new("cmd.exe");
        command
            .args(["/d", "/c", "echo %RUSTPY_PROBE_RAW%& cd"])
            .env("RUSTPY_PROBE_RAW", "raw-env")
            .current_dir(dir.path());
        let output = crate::probe::command_output(command, Duration::from_secs(10)).unwrap();
        let text = String::from_utf8_lossy(&output);
        assert!(text.contains("raw-env"), "{text}");
        let canonical = dunce::canonicalize(dir.path()).unwrap();
        assert!(
            text.to_lowercase()
                .contains(&canonical.display().to_string().to_lowercase()),
            "{text}"
        );
    }
}

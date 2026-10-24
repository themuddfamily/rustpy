//! ConPTY implements portable-pty's interfaces with suspended child creation.
//! The child cannot execute until ProcessTree assigns its kill-on-close job.
use portable_pty::{Child, ChildKiller, ExitStatus, MasterPty, PtySize};
use std::{
    collections::BTreeMap,
    ffi::{OsStr, OsString},
    fs::File,
    io::{self, Read, Write},
    os::windows::{
        ffi::OsStrExt,
        io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle},
    },
    path::Path,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
};
use windows_sys::Win32::{
    Foundation::{HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT},
    System::{
        Console::{ClosePseudoConsole, CreatePseudoConsole, ResizePseudoConsole, COORD, HPCON},
        Pipes::CreatePipe,
        Threading::{
            CreateProcessW, DeleteProcThreadAttributeList, GetExitCodeProcess,
            InitializeProcThreadAttributeList, TerminateProcess, UpdateProcThreadAttribute,
            WaitForSingleObject, CREATE_NEW_PROCESS_GROUP, CREATE_SUSPENDED,
            CREATE_UNICODE_ENVIRONMENT, EXTENDED_STARTUPINFO_PRESENT, INFINITE,
            PROCESS_INFORMATION, PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE, STARTF_USESTDHANDLES,
            STARTUPINFOEXW,
        },
    },
};
pub(crate) struct Parts {
    pub child: Box<dyn Child + Send + Sync>,
    pub reader: Box<dyn Read + Send>,
    pub writer: Box<dyn Write + Send>,
    pub master: Box<dyn MasterPty + Send>,
}
struct Console {
    handle: HPCON,
    closed: AtomicBool,
}
unsafe impl Send for Console {}
unsafe impl Sync for Console {}
impl Console {
    fn close(&self) {
        if !self.closed.swap(true, Ordering::AcqRel) {
            unsafe {
                ClosePseudoConsole(self.handle);
            }
        }
    }
}
impl Drop for Console {
    fn drop(&mut self) {
        self.close();
    }
}
#[derive(Debug)]
struct Process {
    handle: OwnedHandle,
    pid: u32,
}
struct NativeChild {
    process: Arc<Process>,
    console: Arc<Console>,
}
impl std::fmt::Debug for NativeChild {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OwnedConPtyChild")
            .field("pid", &self.process.pid)
            .finish()
    }
}
#[derive(Debug)]
struct Killer(Arc<Process>);
impl ChildKiller for Killer {
    fn kill(&mut self) -> io::Result<()> {
        terminate(&self.0)
    }
    fn clone_killer(&self) -> Box<dyn ChildKiller + Send + Sync> {
        Box::new(Self(self.0.clone()))
    }
}
fn terminate(process: &Process) -> io::Result<()> {
    if unsafe { TerminateProcess(process.handle.as_raw_handle(), 137) } == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}
impl Drop for NativeChild {
    fn drop(&mut self) {
        if !matches!(self.try_wait(), Ok(Some(_))) {
            let _ = terminate(&self.process);
            let _ = self.wait();
        }
        self.console.close();
    }
}

impl ChildKiller for NativeChild {
    fn kill(&mut self) -> io::Result<()> {
        terminate(&self.process)
    }
    fn clone_killer(&self) -> Box<dyn ChildKiller + Send + Sync> {
        Box::new(Killer(self.process.clone()))
    }
}
impl Child for NativeChild {
    fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        let result = unsafe { WaitForSingleObject(self.process.handle.as_raw_handle(), 0) };
        if result == WAIT_TIMEOUT {
            return Ok(None);
        }
        if result != WAIT_OBJECT_0 {
            return Err(io::Error::last_os_error());
        }
        let mut code = 0;
        if unsafe { GetExitCodeProcess(self.process.handle.as_raw_handle(), &mut code) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Some(ExitStatus::with_exit_code(code)))
    }
    fn wait(&mut self) -> io::Result<ExitStatus> {
        if unsafe { WaitForSingleObject(self.process.handle.as_raw_handle(), INFINITE) }
            != WAIT_OBJECT_0
        {
            return Err(io::Error::last_os_error());
        }
        let status = self
            .try_wait()?
            .ok_or_else(|| io::Error::other("ConPTY child did not exit"))?;
        // RunSession calls wait only on its background worker after the
        // process-tree barrier, with the output reader still draining. Closing
        // HPCON here releases the host's pipe writer so that reader reaches EOF.
        self.console.close();
        Ok(status)
    }
    fn process_id(&self) -> Option<u32> {
        Some(self.process.pid)
    }
    fn as_raw_handle(&self) -> Option<RawHandle> {
        Some(self.process.handle.as_raw_handle())
    }
}
struct NativeMaster {
    console: Arc<Console>,
    reader: File,
    writer: Mutex<Option<File>>,
    size: Mutex<PtySize>,
}
impl MasterPty for NativeMaster {
    fn resize(&self, size: PtySize) -> anyhow::Result<()> {
        let result = unsafe {
            ResizePseudoConsole(
                self.console.handle,
                COORD {
                    X: size.cols.min(i16::MAX as u16) as i16,
                    Y: size.rows.min(i16::MAX as u16) as i16,
                },
            )
        };
        anyhow::ensure!(result >= 0, "Cannot resize ConPTY ({result:#x})");
        *self
            .size
            .lock()
            .map_err(|_| anyhow::anyhow!("ConPTY size lock"))? = size;
        Ok(())
    }
    fn get_size(&self) -> anyhow::Result<PtySize> {
        Ok(*self
            .size
            .lock()
            .map_err(|_| anyhow::anyhow!("ConPTY size lock"))?)
    }
    fn try_clone_reader(&self) -> anyhow::Result<Box<dyn Read + Send>> {
        Ok(Box::new(self.reader.try_clone()?))
    }
    fn take_writer(&self) -> anyhow::Result<Box<dyn Write + Send>> {
        Ok(Box::new(
            self.writer
                .lock()
                .map_err(|_| anyhow::anyhow!("ConPTY writer lock"))?
                .take()
                .ok_or_else(|| anyhow::anyhow!("ConPTY writer already taken"))?,
        ))
    }
}
struct Attributes {
    storage: Vec<usize>,
    initialized: bool,
}
impl Attributes {
    fn pointer(&mut self) -> windows_sys::Win32::System::Threading::LPPROC_THREAD_ATTRIBUTE_LIST {
        self.storage.as_mut_ptr().cast()
    }
}
impl Drop for Attributes {
    fn drop(&mut self) {
        if self.initialized {
            unsafe {
                DeleteProcThreadAttributeList(self.pointer());
            }
        }
    }
}
fn wide(value: &OsStr) -> Vec<u16> {
    value.encode_wide().chain(Some(0)).collect()
}
/// Quote each argument using the same backslash/quote rules as Command on Windows.
fn quote(value: &OsStr) -> Vec<u16> {
    let source = value.encode_wide().collect::<Vec<_>>();
    let mut out = vec![b'"' as u16];
    let mut slashes = 0;
    for value in source {
        if value == b'\\' as u16 {
            slashes += 1;
        } else {
            if value == b'"' as u16 {
                out.extend(std::iter::repeat_n(b'\\' as u16, slashes * 2 + 1));
            } else {
                out.extend(std::iter::repeat_n(b'\\' as u16, slashes));
            }
            slashes = 0;
            out.push(value);
        }
    }
    out.extend(std::iter::repeat_n(b'\\' as u16, slashes * 2));
    out.push(b'"' as u16);
    out
}
fn pipe() -> io::Result<(OwnedHandle, OwnedHandle)> {
    let (mut read, mut write): (HANDLE, HANDLE) = (std::ptr::null_mut(), std::ptr::null_mut());
    if unsafe { CreatePipe(&mut read, &mut write, std::ptr::null(), 0) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe {
        (
            OwnedHandle::from_raw_handle(read),
            OwnedHandle::from_raw_handle(write),
        )
    })
}
pub(crate) fn spawn(
    program: &Path,
    args: &[String],
    cwd: &Path,
    env: &BTreeMap<String, String>,
    inherit: bool,
) -> anyhow::Result<Parts> {
    let valid = |value: &OsStr| !value.encode_wide().any(|unit| unit == 0);
    anyhow::ensure!(
        valid(program.as_os_str())
            && valid(cwd.as_os_str())
            && args.iter().all(|arg| valid(OsStr::new(arg))),
        "Process arguments contain NUL"
    );
    anyhow::ensure!(
        env.iter().all(|(key, value)| !key.is_empty()
            && !key.contains('=')
            && valid(OsStr::new(key))
            && valid(OsStr::new(value))),
        "Invalid process environment entry"
    );
    let (input_read, input_write) = pipe()?;
    let (output_read, output_write) = pipe()?;
    let mut console = 0;
    let result = unsafe {
        CreatePseudoConsole(
            COORD { X: 120, Y: 24 },
            input_read.as_raw_handle(),
            output_write.as_raw_handle(),
            0,
            &mut console,
        )
    };
    anyhow::ensure!(result >= 0, "Cannot create ConPTY ({result:#x})");
    let console = Arc::new(Console {
        handle: console,
        closed: AtomicBool::new(false),
    });
    drop(input_read);
    drop(output_write);
    let mut size = 0;
    unsafe {
        InitializeProcThreadAttributeList(std::ptr::null_mut(), 1, 0, &mut size);
    }
    let mut attributes = Attributes {
        storage: vec![0; (size + std::mem::size_of::<usize>() - 1) / std::mem::size_of::<usize>()],
        initialized: false,
    };
    if unsafe { InitializeProcThreadAttributeList(attributes.pointer(), 1, 0, &mut size) } == 0 {
        return Err(io::Error::last_os_error().into());
    }
    attributes.initialized = true;
    if unsafe {
        UpdateProcThreadAttribute(
            attributes.pointer(),
            0,
            PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE as usize,
            console.handle as *const _,
            std::mem::size_of::<HPCON>(),
            std::ptr::null_mut(),
            std::ptr::null(),
        )
    } == 0
    {
        return Err(io::Error::last_os_error().into());
    }
    let mut startup: STARTUPINFOEXW = unsafe { std::mem::zeroed() };
    startup.StartupInfo.cb = std::mem::size_of_val(&startup) as u32;
    // Explicit NULL standard handles prevent redirected host stdio from being
    // inherited past the pseudoconsole; Windows fills them from this ConPTY.
    startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
    startup.lpAttributeList = attributes.pointer();
    let program = crate::process::resolve_program(program);
    let application = wide(program.as_os_str());
    let cwd = wide(cwd.as_os_str());
    let mut command_line = quote(program.as_os_str());
    for arg in args {
        command_line.push(b' ' as u16);
        command_line.extend(quote(OsStr::new(arg)));
    }
    command_line.push(0);
    anyhow::ensure!(
        command_line.len() <= 32767,
        "Windows command line exceeds 32767 UTF-16 units"
    );
    let mut environment = BTreeMap::<String, (OsString, OsString)>::new();
    if inherit {
        for (key, value) in std::env::vars_os() {
            environment.insert(key.to_string_lossy().to_uppercase(), (key, value));
        }
    }
    for (key, value) in env {
        environment.insert(key.to_uppercase(), (key.into(), value.into()));
    }
    environment.insert("TERM".into(), ("TERM".into(), "xterm-256color".into()));
    environment.insert("COLORTERM".into(), ("COLORTERM".into(), "truecolor".into()));
    let mut block = Vec::new();
    for (_, (key, value)) in environment {
        block.extend(key.encode_wide());
        block.push(b'=' as u16);
        block.extend(value.encode_wide());
        block.push(0);
    }
    block.push(0);
    if block.len() == 1 {
        block.push(0);
    }
    let mut information: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
    if unsafe {
        CreateProcessW(
            application.as_ptr(),
            command_line.as_mut_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            0,
            EXTENDED_STARTUPINFO_PRESENT
                | CREATE_UNICODE_ENVIRONMENT
                | CREATE_SUSPENDED
                | CREATE_NEW_PROCESS_GROUP,
            block.as_ptr().cast(),
            cwd.as_ptr(),
            &startup.StartupInfo,
            &mut information,
        )
    } == 0
    {
        return Err(io::Error::last_os_error().into());
    }
    let process = Arc::new(Process {
        handle: unsafe { OwnedHandle::from_raw_handle(information.hProcess) },
        pid: information.dwProcessId,
    });
    let _thread = unsafe { OwnedHandle::from_raw_handle(information.hThread) };
    let child = Box::new(NativeChild {
        process,
        console: console.clone(),
    });
    let reader = File::from(output_read);
    let writer = File::from(input_write);
    let master = Box::new(NativeMaster {
        console,
        reader,
        writer: Mutex::new(Some(writer)),
        size: Mutex::new(PtySize {
            cols: 120,
            rows: 24,
            pixel_width: 0,
            pixel_height: 0,
        }),
    });
    Ok(Parts {
        child,
        reader: master.try_clone_reader()?,
        writer: master.take_writer()?,
        master,
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn windows_argument_quoting_retains_spaces_quotes_and_trailing_slashes() {
        assert_eq!(quote(OsStr::new("a\"b")), vec![34, 97, 92, 34, 98, 34]);
        assert_eq!(quote(OsStr::new("a\\")), vec![34, 97, 92, 92, 34]);
        assert_eq!(
            String::from_utf16(&quote(OsStr::new("a b"))).unwrap(),
            "\"a b\""
        );
    }
    use std::os::windows::process::CommandExt;
    use std::{
        path::PathBuf,
        process::Command,
        time::{Duration, Instant},
    };
    use windows_sys::Win32::{
        Foundation::FILETIME,
        System::{
            Console::{
                GetConsoleMode, GetConsoleScreenBufferInfo, GetStdHandle, ReadConsoleW,
                WriteConsoleW, CONSOLE_SCREEN_BUFFER_INFO, STD_ERROR_HANDLE, STD_INPUT_HANDLE,
                STD_OUTPUT_HANDLE,
            },
            Threading::{
                GetCurrentProcess, GetProcessTimes, OpenProcess, CREATE_NO_WINDOW,
                PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
            },
        },
    };
    const PRODUCER: &str = "windows_pty::tests::conpty_native_process_fixture";
    fn creation(handle: RawHandle) -> u64 {
        let mut values: [FILETIME; 4] = unsafe { std::mem::zeroed() };
        let [created, exited, kernel, user] = &mut values;
        assert_ne!(
            unsafe { GetProcessTimes(handle, created, exited, kernel, user) },
            0
        );
        (u64::from(values[0].dwHighDateTime) << 32) | u64::from(values[0].dwLowDateTime)
    }
    fn ready(path: &Path) {
        let temporary = path.with_extension("pending");
        std::fs::write(
            &temporary,
            format!(
                "{} {}",
                std::process::id(),
                creation(unsafe { GetCurrentProcess() })
            ),
        )
        .unwrap();
        std::fs::rename(temporary, path).unwrap();
    }
    fn console_write(text: &str) {
        let text = text.encode_utf16().collect::<Vec<_>>();
        let mut written = 0;
        assert_ne!(
            unsafe {
                WriteConsoleW(
                    GetStdHandle(STD_OUTPUT_HANDLE),
                    text.as_ptr().cast(),
                    text.len() as u32,
                    &mut written,
                    std::ptr::null(),
                )
            },
            0
        );
        assert_eq!(written, text.len() as u32);
    }
    #[test]
    fn conpty_native_process_fixture() {
        let Some(directory) = std::env::var_os("RUSTPY_WINDOWS_PTY_CHILD") else {
            return;
        };
        let directory = PathBuf::from(directory);
        if std::env::var_os("RUSTPY_WINDOWS_PTY_GRANDCHILD").is_some() {
            ready(&directory.join("grandchild"));
            loop {
                std::thread::sleep(Duration::from_secs(1));
            }
        }
        // The native supervisor redirects its own standard streams to files.
        // Every ConPTY client stream must nevertheless be a real console handle.
        for device in [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
            let mut mode = 0;
            assert_ne!(
                unsafe { GetConsoleMode(GetStdHandle(device), &mut mode) },
                0,
                "ConPTY standard handle {device} bypassed its console: {}",
                io::Error::last_os_error()
            );
        }
        println!("RUSTPY_CONPTY_STDOUT");
        eprintln!("RUSTPY_CONPTY_STDERR");
        // The fixture leader is terminated by the test with its whole console
        // process tree; the grandchild is never individually waited on.
        #[allow(clippy::zombie_processes)]
        let _descendant = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", PRODUCER, "--nocapture", "--test-threads=1"])
            .env("RUSTPY_WINDOWS_PTY_GRANDCHILD", "1")
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .unwrap();
        ready(&directory.join("leader"));
        console_write("RUSTPY_CONPTY_READY\r\n");
        loop {
            let mut text = [0u16; 128];
            let mut read = 0;
            assert_ne!(
                unsafe {
                    ReadConsoleW(
                        GetStdHandle(STD_INPUT_HANDLE),
                        text.as_mut_ptr().cast(),
                        text.len() as u32,
                        &mut read,
                        std::ptr::null_mut(),
                    )
                },
                0
            );
            let text = String::from_utf16(&text[..read as usize]).unwrap();
            let mut dimensions: CONSOLE_SCREEN_BUFFER_INFO = unsafe { std::mem::zeroed() };
            assert_ne!(
                unsafe {
                    GetConsoleScreenBufferInfo(GetStdHandle(STD_OUTPUT_HANDLE), &mut dimensions)
                },
                0
            );
            console_write(&format!(
                "RUSTPY_CONPTY_REPLY={} SIZE={}x{}\r\n",
                text.trim_end_matches(['\r', '\n']),
                dimensions.dwSize.X,
                dimensions.dwSize.Y
            ));
        }
    }
    struct Pinned {
        handle: OwnedHandle,
    }
    impl Pinned {
        fn from_file(path: &Path) -> Option<Self> {
            let value = std::fs::read_to_string(path).ok()?;
            let fields = value.split_whitespace().collect::<Vec<_>>();
            let pid = fields[0].parse::<u32>().unwrap();
            let handle = unsafe {
                OpenProcess(
                    PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
                    0,
                    pid,
                )
            };
            assert!(!handle.is_null());
            let handle = unsafe { OwnedHandle::from_raw_handle(handle) };
            assert_eq!(
                creation(handle.as_raw_handle()),
                fields[1].parse::<u64>().unwrap()
            );
            Some(Self { handle })
        }
        fn retired(&self) -> bool {
            unsafe { WaitForSingleObject(self.handle.as_raw_handle(), 0) == WAIT_OBJECT_0 }
        }
    }
    #[test]
    #[ignore = "requires admitted actual Windows console supervisor and private case"]
    fn conpty_input_output_resize_and_owned_retirement() {
        assert_eq!(
            std::env::var("RUSTPY_WINDOWS_ACCEPTANCE").as_deref(),
            Ok("1")
        );
        let root = PathBuf::from(
            std::env::var_os("RUSTPY_WINDOWS_CASE").expect("private Windows case required"),
        );
        assert!(root.is_absolute() && root.is_dir());
        for drop_session in [false, true] {
            let directory = root.join(format!(
                "conpty-{}-{}",
                std::process::id(),
                u8::from(drop_session)
            ));
            std::fs::create_dir(&directory).unwrap();
            assert!(std::env::var_os("RUSTPY_WINDOWS_PTY_GRANDCHILD").is_none());
            let environment: BTreeMap<String, String> = [(
                "RUSTPY_WINDOWS_PTY_CHILD".into(),
                directory.to_string_lossy().into_owned(),
            )]
            .into_iter()
            .collect();
            let mut session = crate::RunSession::spawn_pty_command_with_environment(
                336,
                "actual native ConPTY",
                &std::env::current_exe().unwrap(),
                &[
                    "--exact".into(),
                    PRODUCER.into(),
                    "--nocapture".into(),
                    "--test-threads=1".into(),
                ],
                &directory,
                &environment,
                true,
            );
            assert!(
                session.is_pty(),
                "actual ConPTY must be admitted: {:?}",
                session.output
            );
            let deadline = Instant::now() + Duration::from_secs(20);
            let (leader, descendant) = loop {
                session.poll();
                if session.console.text().contains("RUSTPY_CONPTY_READY") {
                    if let (Some(leader), Some(descendant)) = (
                        Pinned::from_file(&directory.join("leader")),
                        Pinned::from_file(&directory.join("grandchild")),
                    ) {
                        break (leader, descendant);
                    }
                }
                assert!(
                    session.is_running() && Instant::now() < deadline,
                    "actual ConPTY readiness failed: {:?}",
                    session.output
                );
                std::thread::sleep(Duration::from_millis(5));
            };
            assert!(session.console.text().contains("RUSTPY_CONPTY_STDOUT"));
            assert!(session.console.text().contains("RUSTPY_CONPTY_STDERR"));
            assert!(!leader.retired() && !descendant.retired());
            let stamp = session.lifecycle_stamp();
            let job = stamp
                .split(':')
                .find_map(|part| part.strip_prefix("job="))
                .unwrap()
                .parse::<usize>()
                .unwrap();
            assert_ne!(job, 0);
            for process in [&leader, &descendant] {
                let mut assigned = 0;
                assert_ne!(
                    unsafe {
                        windows_sys::Win32::System::JobObjects::IsProcessInJob(
                            process.handle.as_raw_handle(),
                            job as _,
                            &mut assigned,
                        )
                    },
                    0
                );
                assert_ne!(
                    assigned, 0,
                    "native ConPTY leader/descendant must belong to production Job"
                );
            }

            session.resize_pty(72, 19);
            session.write_stdin("actual π λ\r\n");
            while !session
                .console
                .text()
                .contains("RUSTPY_CONPTY_REPLY=actual π λ SIZE=72x19")
            {
                session.poll();
                assert!(
                    session.is_running() && Instant::now() < deadline,
                    "real ConPTY Unicode/resize roundtrip failed: {:?}",
                    session.output
                );
                std::thread::sleep(Duration::from_millis(5));
            }
            if drop_session {
                drop(session);
            } else {
                let start = Instant::now();
                session.stop();
                assert!(start.elapsed() < Duration::from_millis(100));
                while session.owns_process() {
                    session.poll();
                    assert!(
                        Instant::now() < deadline,
                        "ConPTY Stop/readers did not retire: {}",
                        session.lifecycle_stamp()
                    );
                    std::thread::sleep(Duration::from_millis(5));
                }
                assert!(
                    session.lifecycle_stamp().contains(":readers=0:"),
                    "Stop completion must include reader EOF"
                );
                drop(session);
            }
            while !leader.retired() || !descendant.retired() {
                assert!(
                    Instant::now() < deadline,
                    "actual native ConPTY owners survived Stop/Drop"
                );
                std::thread::sleep(Duration::from_millis(5));
            }
            std::fs::remove_dir_all(directory).unwrap();
        }
    }
}

//! Explicitly admitted native Windows CPython/launcher/registry acceptance.
//! Run one ignored selector inside the reviewed Windows Job supervisor. This
//! target never installs/registers Python or probes every discovered candidate.
#![cfg(windows)]

use rustpy_core::CancellationToken;
use rustpy_project::{interpreter, native_skeleton::supervised_tool};
use sha2::{Digest, Sha256};
use std::{
    ffi::OsString,
    fs,
    io::Read,
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

fn required(key: &str) -> String {
    std::env::var(key)
        .unwrap_or_else(|_| panic!("explicit native Windows prerequisite {key} is required"))
}
fn file(key: &str, console: bool, allow_x86_launcher: bool) -> PathBuf {
    use std::os::windows::fs::MetadataExt;
    let path = PathBuf::from(required(key));
    assert!(
        path.is_absolute(),
        "{key} must select an absolute native path"
    );
    let text = path.to_string_lossy().to_ascii_lowercase();
    assert!(
        !text.contains("\\windowsapps\\"),
        "Store aliases are not admitted SDKs"
    );
    let metadata = fs::symlink_metadata(&path).expect("admitted provider file exists");
    assert!(
        metadata.is_file() && metadata.file_attributes() & 0x400 == 0,
        "{key} must be a real file, never a reparse-point alias"
    );
    let expected = required(&format!("{key}_SHA256"));
    assert!(expected.len() == 64 && expected.bytes().all(|c| c.is_ascii_hexdigit()));
    assert!(
        metadata.len() <= 128 * 1024 * 1024,
        "provider hashing budget"
    );
    let mut reader = fs::File::open(&path).unwrap();
    let mut digest = Sha256::new();
    let mut buffer = [0; 65536];
    loop {
        let count = reader.read(&mut buffer).unwrap();
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    assert_eq!(
        format!("{:x}", digest.finalize()),
        expected.to_ascii_lowercase(),
        "{key} source binding"
    );
    // Verify real native images, not text adapters or GUI executables. Official
    // legacy py.exe can itself be x86 while discovering an AMD64 CPython SDK;
    // only the explicitly admitted launcher may use that genuine PE32 form.
    let bytes = fs::read(&path).unwrap();
    assert_eq!(bytes.get(..2), Some(b"MZ".as_slice()));
    let offset = u32::from_le_bytes(bytes.get(0x3c..0x40).unwrap().try_into().unwrap()) as usize;
    let header = bytes
        .get(offset..offset.checked_add(94).unwrap())
        .expect("bounded PE header");
    assert_eq!(&header[..4], b"PE\0\0");
    let machine = u16::from_le_bytes(header[4..6].try_into().unwrap());
    assert!(
        machine == 0x8664 || (allow_x86_launcher && machine == 0x14c),
        "native AMD64 SDK/system provider or admitted genuine x86 launcher"
    );
    assert_eq!(
        u16::from_le_bytes(header[24..26].try_into().unwrap()),
        if machine == 0x8664 { 0x20b } else { 0x10b }
    );
    let flags = u16::from_le_bytes(header[22..24].try_into().unwrap());
    assert_eq!(flags & 0x2000 != 0, !console, "DLL/executable identity");
    if console {
        assert_eq!(
            u16::from_le_bytes(header[92..94].try_into().unwrap()),
            3,
            "console subsystem; no GUI provider"
        );
    }
    path
}
fn same_file(left: &Path, right: &Path) -> bool {
    dunce::canonicalize(left).expect("actual discovered file exists")
        == dunce::canonicalize(right).expect("admitted file exists")
}
fn contains_sdk(paths: &[PathBuf], sdk: &Path) -> bool {
    paths
        .iter()
        .any(|path| path.is_file() && same_file(path, sdk))
}
struct Environment {
    previous: Vec<(&'static str, Option<OsString>)>,
}
impl Environment {
    fn set(values: &[(&'static str, OsString)]) -> Self {
        // One explicitly selected test/thread in its dedicated supervisor process;
        // no other fixture runs concurrently while these process-local values vary.
        let previous = values
            .iter()
            .map(|(key, _)| (*key, std::env::var_os(key)))
            .collect();
        for (key, value) in values {
            std::env::set_var(key, value);
        }
        Self { previous }
    }
}
impl Drop for Environment {
    fn drop(&mut self) {
        for (key, value) in &self.previous {
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
        }
    }
}
fn supervised(program: &Path, args: &[&std::ffi::OsStr], directory: &Path) -> Vec<u8> {
    let mut command = Command::new(program);
    command.args(args).current_dir(directory);
    let (stdout, stderr) = supervised_tool(
        &mut command,
        &CancellationToken::new(),
        Duration::from_secs(10),
        1024 * 1024,
    )
    .expect("real admitted provider completes successfully under retained process-tree ownership");
    assert!(
        stderr.is_empty(),
        "unexpected admitted provider diagnostic: {}",
        String::from_utf8_lossy(&stderr)
    );
    stdout
}
fn envelope() {
    use std::ffi::c_void;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetCurrentProcess() -> *mut c_void;
        fn GetProcessAffinityMask(
            process: *mut c_void,
            mask: *mut usize,
            system: *mut usize,
        ) -> i32;
        fn GetPriorityClass(process: *mut c_void) -> u32;
        fn IsProcessInJob(process: *mut c_void, job: *mut c_void, result: *mut i32) -> i32;
    }
    assert_eq!(required("RUSTPY_WINDOWS_ACCEPTANCE"), "1");
    assert_eq!(required("RUST_TEST_THREADS"), "1");
    let (mut mask, mut system, mut in_job) = (0usize, 0usize, 0i32);
    unsafe {
        let process = GetCurrentProcess();
        assert_ne!(GetProcessAffinityMask(process, &mut mask, &mut system), 0);
        assert_eq!(mask.count_ones(), 2, "reviewed supervisor two-CPU mask");
        assert_eq!(
            GetPriorityClass(process),
            0x4000,
            "BELOW_NORMAL_PRIORITY_CLASS"
        );
        assert_ne!(
            IsProcessInJob(process, std::ptr::null_mut(), &mut in_job),
            0
        );
    }
    assert_ne!(
        in_job, 0,
        "native Job supervisor must retain physical ownership"
    );
}

#[test]
#[ignore = "requires explicit native Windows Job admission, real registered CPython/py.exe/reg.exe paths+SHA256, version/prefix and a read-only registry InstallPath key; missing prerequisites fail"]
fn configured_windows_registry_launcher_and_cpython_probe_share_real_sdk() {
    envelope();
    let sdk = file("RUSTPY_TEST_WINDOWS_PYTHON", true, false);
    let runtime = file("RUSTPY_TEST_WINDOWS_PYTHON_DLL", false, false);
    let launcher = file("RUSTPY_TEST_WINDOWS_LAUNCHER", true, true);
    let registry = file("RUSTPY_TEST_WINDOWS_REG", true, false);
    assert_eq!(
        sdk.parent(),
        runtime.parent(),
        "runtime DLL belongs to the admitted installation"
    );
    let version = required("RUSTPY_TEST_WINDOWS_VERSION");
    let parts = version
        .split('.')
        .map(|part| part.parse::<u32>().expect("numeric CPython release"))
        .collect::<Vec<_>>();
    assert_eq!(parts.len(), 3);
    assert_eq!(parts[0], 3);
    assert_eq!(
        runtime
            .file_name()
            .unwrap()
            .to_string_lossy()
            .to_ascii_lowercase(),
        format!("python{}{}.dll", parts[0], parts[1])
    );
    let prefix = PathBuf::from(required("RUSTPY_TEST_WINDOWS_PREFIX"));
    assert!(prefix.is_absolute() && prefix.join("Lib").is_dir());
    assert!(same_file(&sdk, &prefix.join("python.exe")));
    let key = required("RUSTPY_TEST_WINDOWS_REGISTRY_KEY");
    assert!(!key.contains(['\r', '\n', '\0']));
    let lower = key.to_ascii_lowercase();
    assert!(
        [
            "hkcu\\software\\python\\",
            "hklm\\software\\python\\",
            "hklm\\software\\wow6432node\\python\\"
        ]
        .iter()
        .any(|hive| lower.starts_with(hive))
            && lower.ends_with("\\installpath"),
        "read-only Python InstallPath authority required"
    );
    let outer = PathBuf::from(required("RUSTPY_WINDOWS_CASE"));
    assert!(outer.is_absolute() && outer.is_dir());
    let case = tempfile::Builder::new()
        .prefix("interpreter-discovery-")
        .tempdir_in(&outer)
        .unwrap();
    let path = std::env::join_paths([
        launcher.parent().unwrap(),
        registry.parent().unwrap(),
        sdk.parent().unwrap(),
    ])
    .unwrap();
    let _environment = Environment::set(&[
        ("PATH", path),
        ("TMP", case.path().as_os_str().to_owned()),
        ("TEMP", case.path().as_os_str().to_owned()),
    ]);
    assert!(same_file(
        &which::which("py.exe").expect("admitted launcher resolves"),
        &launcher
    ));
    assert!(same_file(
        &which::which("reg.exe").expect("admitted registry utility resolves"),
        &registry
    ));
    let launch = supervised(&launcher, &["-0p".as_ref()], case.path());
    fs::write(case.path().join("launcher-readback.txt"), &launch).unwrap();
    let launch_paths = interpreter::windows_launcher_paths(
        std::str::from_utf8(&launch).expect("launcher readback encoding"),
    );
    assert!(
        contains_sdk(&launch_paths, &sdk),
        "real py launcher must advertise the selected SDK"
    );
    let registration = supervised(
        &registry,
        &["query".as_ref(), key.as_ref(), "/ve".as_ref()],
        case.path(),
    );
    fs::write(case.path().join("registry-readback.txt"), &registration).unwrap();
    let registry_paths = interpreter::windows_registry_paths(
        std::str::from_utf8(&registration).expect("registry readback encoding"),
    );
    assert!(
        contains_sdk(&registry_paths, &sdk),
        "actual registered InstallPath must identify the selected SDK"
    );
    // This is the real platform consumer, not parser-only replay. It executes
    // only the admitted launcher/registry read operations, never all candidates.
    let candidates = interpreter::candidate_paths_with(None, true);
    assert!(
        candidates.iter().any(|(path, kind)| path.is_file()
            && same_file(path, &sdk)
            && matches!(kind.as_str(), "Windows py launcher" | "Windows registry")),
        "production platform discovery retains genuine registration origin"
    );
    let metadata = interpreter::probe_detailed(&sdk, "configured Windows acceptance")
        .expect("actual CPython public metadata probe");
    assert_eq!(metadata.interpreter.version, version);
    assert_eq!(metadata.interpreter.platform, "win32");
    assert_eq!(metadata.bits, 64);
    assert!(
        metadata.architecture.eq_ignore_ascii_case("AMD64")
            || metadata.architecture.eq_ignore_ascii_case("x86_64")
    );
    assert!(same_file(&metadata.interpreter.prefix, &prefix));
    assert!(!metadata.interpreter.is_venv);
    assert!(metadata.full_version.contains(&version));
    assert!(metadata
        .interpreter
        .sys_path
        .iter()
        .any(|path| path.is_dir() && same_file(path, &prefix.join("Lib"))));
    assert!(metadata
        .sysconfig
        .get("stdlib")
        .is_some_and(|path| same_file(path, &prefix.join("Lib"))));
    assert!(metadata
        .sysconfig
        .get("purelib")
        .is_some_and(|path| path.starts_with(&prefix)));
    assert_eq!(interpreter::cached_probe(&sdk), Some(Ok(metadata.clone())));
    let script = case.path().join("owned λ.py");
    fs::write(
        &script,
        "import sys\nsys.stdout.reconfigure(encoding='utf-8')\nprint('42 λ 日本語')\n",
    )
    .unwrap();
    let output = supervised(
        &metadata.interpreter.path,
        &["-I".as_ref(), script.as_os_str()],
        case.path(),
    );
    assert_eq!(
        std::str::from_utf8(&output).unwrap().replace("\r\n", "\n"),
        "42 λ 日本語\n"
    );
    fs::write(outer.join("configured-windows-interpreters.json"), serde_json::to_vec_pretty(&serde_json::json!({
        "metadata": metadata, "sdk": sdk, "runtime": runtime, "launcher": launcher, "registry": registry,
        "registry_key": key,
        "provider_sha256": {
            "python": required("RUSTPY_TEST_WINDOWS_PYTHON_SHA256"),
            "runtime": required("RUSTPY_TEST_WINDOWS_PYTHON_DLL_SHA256"),
            "launcher": required("RUSTPY_TEST_WINDOWS_LAUNCHER_SHA256"),
            "registry": required("RUSTPY_TEST_WINDOWS_REG_SHA256")
        },
        "launcher_readback": String::from_utf8(launch).unwrap(),
        "registry_readback": String::from_utf8(registration).unwrap(), "stdout": String::from_utf8(output).unwrap(),
        "scope": "registered installed x64 CPython + production candidate discovery + selected SDK probe/execution; no Store alias execution or other SDK/platform coverage"
    })).unwrap()).unwrap();
    // supervised_tool has joined its owned readers and retired each process
    // tree. The public probe has returned/reaped its direct child. The native
    // outer Job's ActiveProcesses=0 + retained-handle barrier is the additional
    // authoritative whole-process-tree acceptance receipt, not inferred here.
}

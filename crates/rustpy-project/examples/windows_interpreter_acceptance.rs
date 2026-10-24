//! Windows acceptance probe for the Add Interpreter dialog's flows. Every
//! environment is made by `environment_manager::Task`, the backend the
//! dialog's "Create environment" / "Select" / "Reload" buttons submit, and
//! registered through the same `Registry` calls the dialog makes. Run on a
//! real Windows host through `qa/windows/run-windows-acceptance-2.ps1`, which
//! checks the results against Windows itself. Prints one JSON document.
//!
//! Usage: `windows_interpreter_acceptance --work <empty dir> [--base <python.exe>]
//!         [--uv <uv.exe>] [--distro <wsl name>]`
//!
//! Everything (environments, the interpreter catalogue, pip and uv caches) is
//! kept inside `--work`; the caller deletes it.

use rustpy_project::environment_manager::{
    environment_python, plan, registry_path, Event, Manager, Registry, Request, Task,
};
use rustpy_project::interpreter::discover_interpreters_with;
use rustpy_project::interpreter_remote::{ProbeJob, RemoteCandidate, RemoteKind};
use rustpy_project::{Interpreter, ProjectModel};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const PACKAGE: &str = "rustpy_win_probe";
const WHEEL: &str = "rustpy_win_probe-1.0-py3-none-any.whl";

fn main() {
    let mut work = None;
    let mut base = None;
    let mut uv = None;
    let mut distro = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--work" => work = args.next().map(PathBuf::from),
            "--base" => base = args.next().map(PathBuf::from),
            "--uv" => uv = args.next().map(PathBuf::from),
            "--distro" => distro = args.next(),
            other => {
                eprintln!("unknown argument {other}");
                std::process::exit(2);
            }
        }
    }
    let Some(work) = work else {
        eprintln!("--work <dir> is required");
        std::process::exit(2);
    };
    std::fs::create_dir_all(&work).expect("work directory");
    // Absolute but not canonical: no `\\?\` prefix in tool arguments.
    let work = std::path::absolute(&work).expect("work directory");
    // Nothing persistent outside the work folder: the interpreter catalogue,
    // pip's and uv's caches all live in it. No network: pip installs only the
    // wheel generated below and never checks for its own updates.
    std::env::set_var("RUSTPY_CONFIG_DIR", work.join("config"));
    std::env::set_var("PIP_CACHE_DIR", work.join("pip-cache"));
    std::env::set_var("PIP_DISABLE_PIP_VERSION_CHECK", "1");
    std::env::set_var("PIP_NO_INPUT", "1");
    std::env::set_var("UV_CACHE_DIR", work.join("uv-cache"));
    std::env::set_var("UV_PYTHON_DOWNLOADS", "never");
    std::env::remove_var("PYTHONHOME");
    std::env::remove_var("PYTHONPATH");

    let mut report = serde_json::Map::new();
    report.insert("work".into(), json!(work));
    report.insert("registry_path".into(), json!(registry_path()));

    // Discovery, as the dialog's interpreter list shows it.
    let started = Instant::now();
    let discovered = discover_interpreters_with(None, true);
    report.insert(
        "discovered".into(),
        json!(discovered
            .iter()
            .map(|i| json!({"path": i.path, "version": i.version, "kind": i.kind}))
            .collect::<Vec<_>>()),
    );
    report.insert("discovery_ms".into(), json!(started.elapsed().as_millis()));
    let base = base.or_else(|| {
        discovered
            .iter()
            .find(|i| !i.is_venv && i.kind.contains("Windows"))
            .or_else(|| discovered.iter().find(|i| !i.is_venv))
            .map(|i| i.path.clone())
    });
    let Some(base) = base else {
        report.insert("error".into(), json!("no base Python interpreter found"));
        println!("{}", serde_json::to_string_pretty(&report).unwrap());
        return;
    };
    report.insert("base".into(), json!(base));

    // Project folder with a space and a non-ASCII letter, a requirements.txt
    // naming only a locally generated wheel (offline install).
    let project = work.join("managed project λ");
    std::fs::create_dir_all(project.join("wheels")).unwrap();
    std::fs::write(project.join("wheels").join(WHEEL), wheel()).unwrap();
    std::fs::write(
        project.join("requirements.txt"),
        format!("--no-index\n./wheels/{WHEEL}\n"),
    )
    .unwrap();
    report.insert("project".into(), json!(project));
    let trusted = true;
    let request = |manager: Manager, existing: bool, base: &Path, location: &Path| Request {
        root: project.clone(),
        manager,
        existing,
        executable: if manager == Manager::Uv {
            uv.clone().unwrap_or_default()
        } else {
            base.to_path_buf()
        },
        base: base.to_path_buf(),
        location: location.to_path_buf(),
        python_version: String::new(),
        inherit_site_packages: false,
        install: false,
    };

    let mut steps = Vec::new();

    // 1. Virtualenv › New, "Install packages from requirements.txt" on.
    let venv = project.join(".venv");
    let mut create = request(Manager::Virtualenv, false, &base, &venv);
    create.install = true;
    let planned = plan(&create).map(|p| {
        p.commands
            .iter()
            .map(|c| json!({"program": c.program, "args": c.args}))
            .collect::<Vec<_>>()
    });
    let (result, step) = run_task("new_virtualenv", create, trusted, Duration::from_secs(600));
    let mut step = step;
    step["plan"] = match planned {
        Ok(commands) => json!(commands),
        Err(e) => json!({"error": e}),
    };
    step["expected_python"] = json!(environment_python(&venv));
    if let Ok(interpreter) = &result {
        step["prefix_matches_location"] = json!(same_dir(&interpreter.prefix, &venv));
        let python = &interpreter.path;
        step["import_probe"] = json!(run_python(
            python,
            &[
                "-c",
                &format!("import {PACKAGE} as m;print(m.VALUE);print(m.__file__)")
            ],
            &project
        ));
        step["pip_version"] = json!(run_python(python, &["-m", "pip", "--version"], &project));
    }
    steps.push(step);
    let created = result.ok();

    // 2. Virtualenv › Existing: the environment just made.
    let (existing, mut step) = run_task(
        "existing_virtualenv",
        request(Manager::Virtualenv, true, &base, &venv),
        trusted,
        Duration::from_secs(120),
    );
    if let (Ok(found), Some(made)) = (&existing, &created) {
        step["same_as_created"] = json!(found.path == made.path);
    }
    steps.push(step);

    // 3. System interpreter / Reload paths: every discovered system Python
    //    (py launcher, registry, PATH) selected as an existing interpreter.
    let mut systems = Vec::new();
    for interpreter in discovered.iter().filter(|i| !i.is_venv) {
        let (_, step) = run_task(
            &format!("system:{}", interpreter.path.display()),
            request(Manager::System, true, &interpreter.path, &interpreter.path),
            trusted,
            Duration::from_secs(120),
        );
        systems.push(step);
    }
    report.insert("system_interpreters".into(), json!(systems));

    // 4. The py launcher as the base interpreter (its default Python).
    if let Some(py) = rustpy_project::setup::find_on_path("py") {
        let location = project.join(".venv-py");
        let (_, mut step) = run_task(
            "py_launcher_base",
            request(Manager::Virtualenv, false, &py, &location),
            trusted,
            Duration::from_secs(300),
        );
        step["launcher"] = json!(py);
        step["location"] = json!(location);
        steps.push(step);
    } else {
        steps.push(json!({"name": "py_launcher_base", "skipped": "py.exe not on PATH"}));
    }

    // 5. "Inherit global site-packages".
    let inherit_location = project.join(".venv-inherit");
    let mut inherit = request(Manager::Virtualenv, false, &base, &inherit_location);
    inherit.inherit_site_packages = true;
    let (_, mut step) = run_task(
        "inherit_site_packages",
        inherit,
        trusted,
        Duration::from_secs(300),
    );
    step["location"] = json!(inherit_location);
    steps.push(step);

    // 6. A location that already exists is refused before anything runs.
    let refused = plan(&request(Manager::Virtualenv, false, &base, &venv));
    steps.push(json!({
        "name": "existing_location_refused",
        "ok": refused.is_err(),
        "error": refused.err(),
    }));

    // 7. A missing base interpreter fails with a message, not a hang.
    let missing = work.join("no such python").join("python.exe");
    let (bad, mut step) = run_task(
        "missing_base_reports_error",
        request(
            Manager::Virtualenv,
            false,
            &missing,
            &project.join(".venv-missing"),
        ),
        trusted,
        Duration::from_secs(60),
    );
    step["ok"] = json!(bad.is_err());
    steps.push(step);

    // 8. Cancel while `python -m venv` runs: the task ends promptly and its
    //    process tree is gone (the script checks no python.exe still runs).
    let cancel_location = project.join(".venv-cancel");
    let started = Instant::now();
    let cancelled = match Task::start(
        request(Manager::Virtualenv, false, &base, &cancel_location),
        trusted,
    ) {
        Err(e) => json!({"name": "cancel", "ok": false, "error": e}),
        Ok(task) => {
            std::thread::sleep(Duration::from_millis(300));
            task.cancel();
            let outcome = wait(&task, Duration::from_secs(60));
            json!({
                "name": "cancel",
                "ok": matches!(outcome, Some(Err(_))),
                "ms": started.elapsed().as_millis(),
                "error": outcome.and_then(|r| r.err()),
                "location": cancel_location,
            })
        }
    };
    steps.push(cancelled);

    // 9. uv › New (optional: uv on PATH or --uv).
    let uv_tool = uv
        .clone()
        .or_else(|| rustpy_project::setup::find_on_path("uv"));
    if let Some(tool) = uv_tool {
        let location = project.join(".venv-uv");
        let mut r = request(Manager::Uv, false, &base, &location);
        r.executable = tool.clone();
        let (_, mut step) = run_task("new_uv", r, trusted, Duration::from_secs(600));
        step["uv"] = json!(tool);
        steps.push(step);
    } else {
        steps.push(json!({"name": "new_uv", "skipped": "uv not on PATH"}));
    }

    // 10. Optional WSL interpreter through Add Interpreter › WSL, then
    //     selected again as a configured interpreter (its .cmd wrapper).
    if let Some(distro) = distro {
        let candidate = RemoteCandidate {
            kind: RemoteKind::Wsl,
            target: distro.clone(),
            python: "python3".into(),
            project_root: project.clone(),
            docker: None,
            remote_root: None,
        };
        let started = Instant::now();
        let outcome = match ProbeJob::remote(candidate, work.join("config").join("adapters"), true)
        {
            Err(e) => Err(e),
            Ok(job) => job
                .receiver
                .recv_timeout(Duration::from_secs(120))
                .map_err(|_| "no result within 120 s".to_string())
                .and_then(|r| r),
        };
        let mut step =
            json!({"name": "wsl", "distro": distro, "ms": started.elapsed().as_millis()});
        match outcome {
            Err(e) => {
                step["ok"] = json!(false);
                step["error"] = json!(e);
            }
            Ok(m) => {
                step["ok"] = json!(true);
                step["interpreter"] = describe(&m.interpreter);
                let wrapper = m.interpreter.path.clone();
                let (_, again) = run_task(
                    "wsl_reselected",
                    request(Manager::System, true, &wrapper, &wrapper),
                    trusted,
                    Duration::from_secs(120),
                );
                step["reselected"] = again;
            }
        }
        steps.push(step);
    }

    // 11. Catalogue: name, associate, rename, remove, exactly as the dialog's
    //     Interpreter ready / Rename / "Remove from configured list" do, plus
    //     the project's SDK as Select writes it.
    if let Some(interpreter) = created.clone() {
        steps.push(catalogue(interpreter, &project));
    }

    report.insert("steps".into(), json!(steps));
    println!("{}", serde_json::to_string_pretty(&report).unwrap());
}

fn catalogue(interpreter: Interpreter, project: &Path) -> Value {
    let path = registry_path();
    let mut checks = serde_json::Map::new();
    let mut registry = Registry::load(&path);
    registry.remember(interpreter.clone(), project, "Windows venv λ", false);
    let mut applied = interpreter.clone();
    registry.apply_paths(&mut applied);
    let saved = registry.save(&path);
    checks.insert("saved".into(), json!(saved.is_ok()));
    checks.insert("save_error".into(), json!(saved.err()));
    let loaded = Registry::load(&path);
    let entry = loaded
        .entries
        .iter()
        .find(|e| e.interpreter.path == interpreter.path);
    checks.insert("reloaded_name".into(), json!(entry.map(|e| e.name.clone())));
    checks.insert(
        "reloaded_project".into(),
        json!(entry.is_some_and(|e| e.projects.iter().any(|p| p == project))),
    );

    // Select: the project's SDK.
    let mut model = ProjectModel::load(project);
    model.sdk = Some(interpreter.path.clone());
    let sdk_saved = model.save(project);
    checks.insert("sdk_saved".into(), json!(sdk_saved.is_ok()));
    checks.insert(
        "sdk_reloaded".into(),
        json!(ProjectModel::load(project).sdk == Some(interpreter.path.clone())),
    );

    // Rename.
    let mut registry = Registry::load(&path);
    registry.remember(interpreter.clone(), project, "Renamed λ", false);
    let _ = registry.save(&path);
    checks.insert(
        "renamed".into(),
        json!(Registry::load(&path)
            .entries
            .iter()
            .find(|e| e.interpreter.path == interpreter.path)
            .map(|e| e.name.clone())),
    );

    // Remove from configured list.
    let mut registry = Registry::load(&path);
    if let Some(index) = registry
        .entries
        .iter()
        .position(|e| e.interpreter.path == interpreter.path)
    {
        let removed = registry.entries.remove(index);
        registry.hidden_paths.push(removed.interpreter.path);
    }
    let removed = registry.save(&path);
    let after = Registry::load(&path);
    checks.insert("remove_saved".into(), json!(removed.is_ok()));
    checks.insert(
        "removed".into(),
        json!(!after
            .entries
            .iter()
            .any(|e| e.interpreter.path == interpreter.path)),
    );
    checks.insert(
        "hidden".into(),
        json!(after.hidden_paths.contains(&interpreter.path)),
    );
    checks.insert("registry_file".into(), json!(path));
    let ok = checks["saved"] == json!(true)
        && checks["reloaded_name"] == json!("Windows venv λ")
        && checks["reloaded_project"] == json!(true)
        && checks["sdk_reloaded"] == json!(true)
        && checks["renamed"] == json!("Renamed λ")
        && checks["removed"] == json!(true)
        && checks["hidden"] == json!(true);
    checks.insert("name".into(), json!("catalogue"));
    checks.insert("ok".into(), json!(ok));
    Value::Object(checks)
}

fn wait(task: &Task, limit: Duration) -> Option<Result<Interpreter, String>> {
    let deadline = Instant::now() + limit;
    while Instant::now() < deadline {
        match task.poll() {
            Some(Event::Finished(result)) => return Some(result),
            Some(Event::Progress(_)) => {}
            None => std::thread::sleep(Duration::from_millis(20)),
        }
    }
    task.cancel();
    None
}

fn run_task(
    name: &str,
    request: Request,
    trusted: bool,
    limit: Duration,
) -> (Result<Interpreter, String>, Value) {
    let started = Instant::now();
    let result = match Task::start(request, trusted) {
        Err(e) => Err(e),
        Ok(task) => wait(&task, limit)
            .unwrap_or_else(|| Err(format!("no result within {} s", limit.as_secs()))),
    };
    let ms = started.elapsed().as_millis();
    let step = match &result {
        Ok(interpreter) => json!({
            "name": name, "ok": true, "ms": ms, "interpreter": describe(interpreter),
        }),
        Err(e) => json!({"name": name, "ok": false, "ms": ms, "error": e}),
    };
    (result, step)
}

fn describe(i: &Interpreter) -> Value {
    json!({
        "path": i.path,
        "version": i.version,
        "prefix": i.prefix,
        "is_venv": i.is_venv,
        "kind": i.kind,
        "platform": i.platform,
        "sys_path_entries": i.sys_path.len(),
        "path_is_file": i.path.is_file(),
    })
}

fn same_dir(a: &Path, b: &Path) -> bool {
    match (dunce::canonicalize(a), dunce::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

fn run_python(python: &Path, args: &[&str], cwd: &Path) -> Value {
    let mut command = rustpy_core::interpreter_launch::interpreter_command(python);
    command
        .args(args)
        .current_dir(cwd)
        .env("PYTHONIOENCODING", "utf-8");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000);
    }
    match command.output() {
        Ok(out) => json!({
            "status": out.status.code(),
            "stdout": String::from_utf8_lossy(&out.stdout).trim(),
            "stderr": String::from_utf8_lossy(&out.stderr).trim(),
        }),
        Err(e) => json!({"error": e.to_string()}),
    }
}

/// A pure-Python wheel built in memory (stored zip), so pip installs it with
/// `--no-index` and no network.
fn wheel() -> Vec<u8> {
    let dist = "rustpy_win_probe-1.0.dist-info";
    let mut files: Vec<(String, Vec<u8>)> = vec![
        (
            format!("{PACKAGE}.py"),
            b"VALUE = 42\n__version__ = \"1.0\"\n".to_vec(),
        ),
        (
            format!("{dist}/METADATA"),
            b"Metadata-Version: 2.1\nName: rustpy-win-probe\nVersion: 1.0\nSummary: RustPy Windows acceptance probe\n".to_vec(),
        ),
        (
            format!("{dist}/WHEEL"),
            b"Wheel-Version: 1.0\nGenerator: rustpy-qa\nRoot-Is-Purelib: true\nTag: py3-none-any\n"
                .to_vec(),
        ),
    ];
    let mut record = String::new();
    for (name, data) in &files {
        record.push_str(&format!(
            "{name},sha256={},{}\n",
            base64_url(&Sha256::digest(data)),
            data.len()
        ));
    }
    record.push_str(&format!("{dist}/RECORD,,\n"));
    files.push((format!("{dist}/RECORD"), record.into_bytes()));
    stored_zip(&files)
}

fn base64_url(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let n = (chunk[0] as u32) << 16
            | (*chunk.get(1).unwrap_or(&0) as u32) << 8
            | *chunk.get(2).unwrap_or(&0) as u32;
        for i in 0..chunk.len() + 1 {
            out.push(TABLE[(n >> (18 - 6 * i) & 63) as usize] as char);
        }
    }
    out
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &byte in data {
        crc ^= byte as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

fn stored_zip(files: &[(String, Vec<u8>)]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut central = Vec::new();
    let le16 = |v: &mut Vec<u8>, n: u16| v.extend_from_slice(&n.to_le_bytes());
    let le32 = |v: &mut Vec<u8>, n: u32| v.extend_from_slice(&n.to_le_bytes());
    for (name, data) in files {
        let offset = out.len() as u32;
        let crc = crc32(data);
        // Local file header: version 2.0, no flags, stored, 1980-01-01.
        le32(&mut out, 0x0403_4b50);
        for n in [20, 0, 0, 0, 0x21] {
            le16(&mut out, n);
        }
        le32(&mut out, crc);
        le32(&mut out, data.len() as u32);
        le32(&mut out, data.len() as u32);
        le16(&mut out, name.len() as u16);
        le16(&mut out, 0);
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(data);
        le32(&mut central, 0x0201_4b50);
        for n in [20, 20, 0, 0, 0, 0x21] {
            le16(&mut central, n);
        }
        le32(&mut central, crc);
        le32(&mut central, data.len() as u32);
        le32(&mut central, data.len() as u32);
        le16(&mut central, name.len() as u16);
        for n in [0, 0, 0, 0] {
            le16(&mut central, n);
        }
        le32(&mut central, 0);
        le32(&mut central, offset);
        central.extend_from_slice(name.as_bytes());
    }
    let start = out.len() as u32;
    out.extend_from_slice(&central);
    le32(&mut out, 0x0605_4b50);
    le16(&mut out, 0);
    le16(&mut out, 0);
    le16(&mut out, files.len() as u16);
    le16(&mut out, files.len() as u16);
    le32(&mut out, central.len() as u32);
    le32(&mut out, start);
    le16(&mut out, 0);
    out
}

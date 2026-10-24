//! Explicit diagnostic captures for this IDE process, with supervised tool ownership.
#[cfg(all(test, target_os = "linux"))]
mod configured_tests;
use rustpy_core::{ProgressReporter, TaskError, TaskResult};
use std::{
    io::Read,
    path::{Path, PathBuf},
    process::Stdio,
    time::{Duration, Instant},
};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Threads,
    Profile,
}
#[derive(Debug)]
pub struct Artifact {
    pub path: PathBuf,
    pub text: Option<String>,
}
#[derive(Clone, Debug)]
pub struct Plan {
    pub program: PathBuf,
    pub arguments: Vec<String>,
    pub output: PathBuf,
    pub kind: Kind,
    pub seconds: u32,
}
pub fn plan(
    kind: Kind,
    settings: &rustpy_settings::diagnostics::Settings,
    pid: u32,
    output: PathBuf,
) -> Result<Plan, String> {
    let program = if kind == Kind::Profile {
        settings.profiler_program.trim()
    } else {
        settings.debugger_program.trim()
    };
    if program.is_empty() || program.contains('\0') {
        return Err("Configure the diagnostic tool executable in Settings".into());
    }
    let seconds = settings.profile_seconds.clamp(1, 60);
    let arguments = if kind == Kind::Profile {
        vec![
            "record".into(),
            "--pid".into(),
            pid.to_string(),
            "--save-only".into(),
            "--rate".into(),
            "49".into(),
            "--duration".into(),
            seconds.to_string(),
            "--output".into(),
            output.to_string_lossy().into_owned(),
        ]
    } else {
        let name = Path::new(program)
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .to_ascii_lowercase();
        if name.contains("lldb") {
            vec![
                "--batch".into(),
                "--attach-pid".into(),
                pid.to_string(),
                "-o".into(),
                "thread backtrace all".into(),
                "-o".into(),
                "detach".into(),
            ]
        } else if name.contains("cdb") || name.contains("windbg") {
            vec![
                "-p".into(),
                pid.to_string(),
                "-c".into(),
                "~* kb; .detach; q".into(),
            ]
        } else {
            vec![
                "--batch".into(),
                "--nx".into(),
                "--pid".into(),
                pid.to_string(),
                "-ex".into(),
                "set pagination off".into(),
                "-ex".into(),
                "thread apply all bt".into(),
                "-ex".into(),
                "detach".into(),
            ]
        }
    };
    Ok(Plan {
        program: program.into(),
        arguments,
        output,
        kind,
        seconds,
    })
}
struct ChildOwner {
    child: std::process::Child,
    tree: rustpy_run::ProcessTree,
}
impl Drop for ChildOwner {
    fn drop(&mut self) {
        self.tree.force(&mut self.child);
    }
}
fn reader(input: impl Read + Send + 'static) -> std::thread::JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut input = input;
        let mut bytes = Vec::new();
        let mut buffer = [0u8; 4096];
        while let Ok(count) = input.read(&mut buffer) {
            if count == 0 {
                break;
            }
            let take = count.min((256 * 1024usize).saturating_sub(bytes.len()));
            bytes.extend_from_slice(&buffer[..take]);
        }
        bytes
    })
}
pub fn capture(plan: Plan, progress: &ProgressReporter) -> TaskResult<Artifact> {
    progress.check()?;
    let mut command = rustpy_core::interpreter_launch::interpreter_command(&plan.program);
    command
        .args(&plan.arguments)
        .current_dir(plan.output.parent().unwrap_or(Path::new(".")))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    rustpy_run::ProcessTree::prepare(&mut command);
    let mut child = command.spawn().map_err(|error| {
        TaskError::Failed(format!("Cannot start {}: {error}", plan.program.display()))
    })?;
    let tree = match rustpy_run::ProcessTree::new(&mut child) {
        Ok(tree) => tree,
        Err(error) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(TaskError::Failed(error.to_string()));
        }
    };
    let stdout = child.stdout.take().map(reader);
    let stderr = child.stderr.take().map(reader);
    let mut owner = ChildOwner { child, tree };
    let started = Instant::now();
    let mut interrupted = None;
    let mut cancelled = false;
    let status = loop {
        let state = owner
            .tree
            .poll(&mut owner.child)
            .map_err(|e| TaskError::Failed(e.to_string()))?;
        if let Some(status) = state {
            break Some(status);
        }
        cancelled |= progress.is_cancelled();
        let timed_out = started.elapsed()
            >= Duration::from_secs(if plan.kind == Kind::Profile {
                u64::from(plan.seconds)
            } else {
                20
            });
        if (cancelled || timed_out) && interrupted.is_none() {
            let _ = owner.tree.interrupt(&mut owner.child);
            interrupted = Some(Instant::now());
        }
        if interrupted.is_some_and(|at| at.elapsed() >= Duration::from_secs(8)) {
            owner.tree.force(&mut owner.child);
            break None;
        }
        progress.set_text(if plan.kind == Kind::Profile {
            "Recording IDE CPU stacks"
        } else {
            "Capturing IDE thread stacks"
        });
        std::thread::sleep(Duration::from_millis(30));
    };
    drop(owner);
    let stdout = stdout
        .and_then(|reader| reader.join().ok())
        .unwrap_or_default();
    let stderr = stderr
        .and_then(|reader| reader.join().ok())
        .unwrap_or_default();
    if cancelled || progress.is_cancelled() {
        let _ = std::fs::remove_file(&plan.output);
        return Err(TaskError::Cancelled);
    }
    let output = format!(
        "{}\n{}",
        String::from_utf8_lossy(&stdout),
        String::from_utf8_lossy(&stderr)
    );
    if plan.kind == Kind::Profile {
        validate_profile(&plan.output)
            .map_err(|error| TaskError::Failed(format!("{error}\n{output}")))?;
        Ok(Artifact {
            path: plan.output,
            text: None,
        })
    } else {
        if !status.is_some_and(|status| status.success()) {
            return Err(TaskError::Failed(format!("Thread capture failed; check debugger installation and OS attach permission. {output}")));
        }
        if stdout.is_empty() {
            return Err(TaskError::Failed(
                "The debugger returned no thread stacks".into(),
            ));
        }
        std::fs::write(&plan.output, output.as_bytes())
            .map_err(|error| TaskError::Failed(error.to_string()))?;
        Ok(Artifact {
            path: plan.output,
            text: Some(output),
        })
    }
}
pub fn validate_profile(path: &Path) -> Result<(), String> {
    let metadata = std::fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    if !metadata.is_file() || metadata.len() > 128 * 1024 * 1024 {
        return Err("The profiler did not produce a bounded regular profile file".into());
    }
    let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let input: Box<dyn Read> = if path.extension().is_some_and(|ext| ext == "gz") {
        Box::new(flate2::read::GzDecoder::new(file))
    } else {
        Box::new(file)
    };
    let mut bytes = Vec::new();
    input
        .take(64 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > 64 * 1024 * 1024 {
        return Err("The decoded CPU profile exceeds 64 MiB".into());
    }
    let profile: serde_json::Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    if !profile
        .get("meta")
        .is_some_and(serde_json::Value::is_object)
        || !profile
            .get("threads")
            .is_some_and(serde_json::Value::is_array)
    {
        return Err("The sampler did not produce a Firefox CPU profile".into());
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn capture_plans_keep_executable_and_output_as_owned_arguments() {
        let settings = rustpy_settings::diagnostics::Settings {
            profiler_program: "/tools with spaces/samply".into(),
            profile_seconds: 999,
            ..Default::default()
        };
        let profile = plan(
            Kind::Profile,
            &settings,
            42,
            "/private logs/profile.json.gz".into(),
        )
        .unwrap();
        assert_eq!(profile.program, Path::new("/tools with spaces/samply"));
        assert_eq!(profile.seconds, 60);
        assert!(profile
            .arguments
            .windows(2)
            .any(|pair| pair == ["--pid", "42"]));
        assert_eq!(
            profile.arguments.last().unwrap(),
            "/private logs/profile.json.gz"
        );
        assert!(!profile.arguments.iter().any(|argument| argument == "--all"));
    }
    #[test]
    fn profile_validation_accepts_real_shape_and_rejects_nonprofiles_and_symlinks() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("profile.json");
        std::fs::write(&path, r#"{"meta":{"version":24},"threads":[]}"#).unwrap();
        validate_profile(&path).unwrap();
        std::fs::write(&path, r#"{"message":"tool failed"}"#).unwrap();
        assert!(validate_profile(&path).is_err());
        #[cfg(unix)]
        {
            let link = dir.path().join("link.json");
            std::os::unix::fs::symlink(&path, &link).unwrap();
            assert!(validate_profile(&link).is_err());
        }
    }
}

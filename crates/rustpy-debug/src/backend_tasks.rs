//! Explicit background debugger installation and process discovery. No command
//! runs from a render callback; cancellation owns exact child cleanup.
use rustpy_core::CancellationToken;
use std::{path::PathBuf, process::Command, time::Duration};
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PythonProcess {
    pub pid: u32,
    pub executable: String,
    pub command: String,
}
#[derive(Clone, Debug)]
pub enum TaskResult {
    Installed(PathBuf),
    Processes(Vec<PythonProcess>),
}
pub struct BackendTask {
    pub owner_id: u64,
    cancel: CancellationToken,
    receiver: crossbeam_channel::Receiver<Result<TaskResult, String>>,
}
impl BackendTask {
    pub fn install(interpreter: PathBuf) -> Self {
        Self::spawn(move |cancel| {
            let mut command = rustpy_core::interpreter_launch::interpreter_command(&interpreter);
            command.args(["-m", "pip", "install", "debugpy"]);
            rustpy_project::native_skeleton::supervised_tool(
                &mut command,
                cancel,
                Duration::from_secs(180),
                4 * 1024 * 1024,
            )
            .map_err(|error| error.to_string())?;
            Ok(TaskResult::Installed(interpreter))
        })
    }
    pub fn processes() -> Self {
        Self::spawn(move |cancel| {
            #[cfg(unix)]
            let mut command = {
                let mut command = Command::new("ps");
                command.args(["-eo", "pid=,comm=,args="]);
                command
            };
            #[cfg(windows)]
            let mut command = {
                let mut command = Command::new("powershell.exe");
                command.args(["-NoProfile","-NonInteractive","-Command","Get-CimInstance Win32_Process | Where-Object { $_.Name -match '^(python|pythonw|pypy).*\\.exe$' } | Select-Object ProcessId,ExecutablePath,CommandLine | ConvertTo-Json -Compress"]);
                command
            };
            let (stdout, _) = rustpy_project::native_skeleton::supervised_tool(
                &mut command,
                cancel,
                Duration::from_secs(10),
                4 * 1024 * 1024,
            )
            .map_err(|error| error.to_string())?;
            Ok(TaskResult::Processes(parse_processes(
                &String::from_utf8_lossy(&stdout),
            )))
        })
    }
    fn spawn(
        work: impl FnOnce(&CancellationToken) -> Result<TaskResult, String> + Send + 'static,
    ) -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let owner_id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let cancel = CancellationToken::new();
        let token = cancel.clone();
        let (sender, receiver) = crossbeam_channel::bounded(1);
        std::thread::spawn(move || {
            let _ = sender.send(work(&token));
        });
        Self {
            owner_id,
            cancel,
            receiver,
        }
    }
    pub fn poll(&self) -> Option<Result<TaskResult, String>> {
        self.receiver.try_recv().ok()
    }
}
impl Drop for BackendTask {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}
pub fn parse_processes(text: &str) -> Vec<PythonProcess> {
    if text.trim_start().starts_with(['[', '{']) {
        let value = serde_json::from_str::<serde_json::Value>(text).unwrap_or_default();
        let entries = value.as_array().cloned().unwrap_or_else(|| vec![value]);
        return entries
            .into_iter()
            .filter_map(|value| {
                Some(PythonProcess {
                    pid: u32::try_from(value["ProcessId"].as_u64()?).ok()?,
                    executable: value["ExecutablePath"].as_str().unwrap_or("").into(),
                    command: value["CommandLine"]
                        .as_str()
                        .unwrap_or("")
                        .chars()
                        .take(4096)
                        .collect(),
                })
            })
            .take(4096)
            .collect();
    }
    text.lines()
        .filter_map(|line| {
            let mut parts = line.trim().splitn(2, char::is_whitespace);
            let pid = parts.next()?.parse().ok()?;
            let rest = parts.next()?.trim_start();
            let (executable, command) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
            let name = executable.to_ascii_lowercase();
            if !name.starts_with("python") && !name.starts_with("pypy") {
                return None;
            }
            Some(PythonProcess {
                pid,
                executable: executable.into(),
                command: command.trim().chars().take(4096).collect(),
            })
        })
        .take(4096)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unix_inventory_uses_executable_not_incidental_arguments() {
        let rows=parse_processes("12 python3 python3 worker.py\n13 sh sh -c python3 worker.py\n14 pypy3 pypy3 server.py\n");
        assert_eq!(
            rows.iter().map(|row| row.pid).collect::<Vec<_>>(),
            vec![12, 14]
        );
        assert_eq!(rows[0].command, "python3 worker.py");
    }
    #[test]
    fn windows_single_process_inventory_preserves_spaces() {
        let rows = parse_processes(
            r#"{"ProcessId":42,"ExecutablePath":"C:\\Python\\python.exe","CommandLine":"python.exe -m app"}"#,
        );
        assert_eq!(rows[0].pid, 42);
        assert_eq!(rows[0].command, "python.exe -m app");
    }
}

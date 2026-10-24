//! Cancellable owned notebook installation/export commands; no shell interpretation.
use std::{
    io::{Read, Write},
    path::Path,
    process::Stdio,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};
pub struct ProcessTask {
    pub id: u64,
    cancel: Arc<AtomicBool>,
    rx: crossbeam_channel::Receiver<Result<String, String>>,
    pub label: String,
    pub started: Instant,
    pub result: Option<Result<String, String>>,
}
impl ProcessTask {
    pub fn start(program: &Path, args: Vec<String>, cwd: &Path, label: impl Into<String>) -> Self {
        Self::start_with_cleanup(program, args, cwd, label, Vec::new())
    }
    pub fn start_with_cleanup(
        program: &Path,
        args: Vec<String>,
        cwd: &Path,
        label: impl Into<String>,
        cleanup: Vec<std::path::PathBuf>,
    ) -> Self {
        let (cancel, worker_cancel) = {
            let value = Arc::new(AtomicBool::new(false));
            (value.clone(), value)
        };
        let (tx, rx) = crossbeam_channel::bounded(1);
        let program = program.to_owned();
        let cwd = cwd.to_owned();
        std::thread::spawn(move || {
            let result = (|| -> Result<String, String> {
                let mut command = rustpy_core::interpreter_launch::interpreter_command(&program);
                command
                    .args(args)
                    .current_dir(cwd)
                    .stdin(Stdio::null())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped());
                #[cfg(unix)]
                {
                    use std::os::unix::process::CommandExt;
                    unsafe {
                        command.pre_exec(|| {
                            if libc::setsid() < 0 {
                                return Err(std::io::Error::last_os_error());
                            }
                            Ok(())
                        });
                    }
                }
                let mut child = command.spawn().map_err(|error| error.to_string())?;
                let out = child.stdout.take().unwrap();
                let err = child.stderr.take().unwrap();
                let read = |mut stream: Box<dyn Read + Send>| {
                    std::thread::spawn(move || {
                        let mut bytes = Vec::new();
                        let _ = stream
                            .by_ref()
                            .take(2 * 1024 * 1024)
                            .read_to_end(&mut bytes);
                        String::from_utf8_lossy(&bytes).into_owned()
                    })
                };
                let stdout = read(Box::new(out));
                let stderr = read(Box::new(err));
                let deadline = Instant::now() + Duration::from_secs(600);
                let status = loop {
                    if worker_cancel.load(Ordering::Acquire) || Instant::now() > deadline {
                        #[cfg(unix)]
                        unsafe {
                            libc::kill(-(child.id() as i32), libc::SIGKILL);
                        }
                        let _ = child.kill();
                        let _ = child.wait();
                        let _ = stdout.join();
                        let _ = stderr.join();
                        return Err("Notebook operation cancelled or timed out".into());
                    }
                    match child.try_wait() {
                        Ok(Some(status)) => break status,
                        Ok(None) => std::thread::sleep(Duration::from_millis(20)),
                        Err(error) => {
                            let _ = child.kill();
                            let _ = child.wait();
                            return Err(error.to_string());
                        }
                    }
                };
                let log = format!(
                    "{}{}",
                    stdout.join().unwrap_or_default(),
                    stderr.join().unwrap_or_default()
                );
                if status.success() {
                    Ok(log)
                } else {
                    Err(format!("Notebook command exited {status}: {log}"))
                }
            })();
            for path in cleanup {
                let _ = std::fs::remove_file(path);
            }
            let _ = tx.send(result);
        });
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        Self {
            id: NEXT.fetch_add(1, Ordering::Relaxed),
            cancel,
            rx,
            label: label.into(),
            started: Instant::now(),
            result: None,
        }
    }
    pub fn poll(&mut self) -> bool {
        if self.result.is_none() {
            match self.rx.try_recv() {
                Ok(result) => {
                    self.result = Some(result);
                    return true;
                }
                Err(crossbeam_channel::TryRecvError::Disconnected) => {
                    self.result = Some(Err("Notebook operation worker stopped".into()));
                    return true;
                }
                _ => {}
            }
        }
        false
    }
    pub fn running(&self) -> bool {
        self.result.is_none()
    }
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Release);
    }
}
impl Drop for ProcessTask {
    fn drop(&mut self) {
        self.cancel();
    }
}
pub fn export_html(notebook: &crate::Notebook, trusted: bool) -> String {
    fn escape(text: &str) -> String {
        text.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;")
    }
    let mut html=String::from("<!doctype html><meta charset=\"utf-8\"><title>Notebook</title><style>body{max-width:1000px;margin:auto;font:16px sans-serif}pre{white-space:pre-wrap;background:#eee;padding:12px}.error{color:#a00}</style>");
    for cell in &notebook.cells {
        if cell.kind == crate::CellKind::Markdown && trusted {
            html.push_str("<section>");
            pulldown_cmark::html::push_html(&mut html, pulldown_cmark::Parser::new(&cell.source));
        } else {
            html.push_str(&format!("<section><pre>{}</pre>", escape(&cell.source)));
        }
        for output in &cell.outputs {
            match output {
                crate::Output::Stream { text, .. } => {
                    html.push_str(&format!("<pre>{}</pre>", escape(text)))
                }
                crate::Output::Error { ename, evalue, .. } => html.push_str(&format!(
                    "<pre class=error>{}: {}</pre>",
                    escape(ename),
                    escape(evalue)
                )),
                crate::Output::DisplayData { data, .. }
                | crate::Output::ExecuteResult { data, .. } => {
                    if trusted && data.contains_key("text/html") {
                        html.push_str(data.get("text/html").unwrap());
                    } else if let Some((mime, image)) = data
                        .get("image/png")
                        .map(|value| ("image/png", value))
                        .or_else(|| data.get("image/jpeg").map(|value| ("image/jpeg", value)))
                    {
                        html.push_str(&format!(
                            "<img alt=\"Notebook output\" src=\"data:{mime};base64,{}\">",
                            escape(image)
                        ))
                    } else {
                        html.push_str(&format!(
                            "<pre>{}</pre>",
                            escape(
                                data.get("text/plain")
                                    .map(String::as_str)
                                    .unwrap_or_default()
                            )
                        ))
                    }
                }
                _ => {}
            }
        }
        html.push_str("</section>");
    }
    html
}
pub fn write_export(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|error| error.to_string())?;
    file.write_all(bytes).map_err(|error| error.to_string())?;
    file.sync_all().map_err(|error| error.to_string())
}

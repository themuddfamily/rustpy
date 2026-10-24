//! Explicit trusted Python scripts run under the existing supervised process
//! owner. This is an IDE API bridge, not an OS sandbox like the WASM host.
use super::*;
use crate::a11y::DialogWindow;
use rustpy_plugins::runtime::{self, Capability, Request, Response, RuntimeManifest};
use std::{collections::BTreeSet, io::Write, time::Duration};
#[derive(Clone, PartialEq, Eq)]
struct Authority {
    root: PathBuf,
    workspace: u64,
    interpreter: PathBuf,
    source: String,
}
const SCRIPT_JOB: u64 = 1 << 63;
pub(super) struct Pending {
    task: rustpy_core::TaskHandle<Response>,
    request: Request,
    root: PathBuf,
    workspace: u64,
    document: Option<(rustpy_text::DocumentId, u64, usize)>,
    stopping: bool,
    authority: Authority,
}
pub(super) struct State {
    pub source: String,
    consent: bool,
    consent_scope: Option<Authority>,
    pending: Option<Pending>,
    message: String,
}
impl Drop for State {
    fn drop(&mut self) {
        if let Some(pending) = &self.pending {
            pending.task.cancel();
        }
    }
}
impl Default for State {
    fn default() -> Self {
        Self{source:"ide.notify('Hello from the IDE API')\nif ide.document:\n    print(ide.document['path'])\n".into(),consent:false,consent_scope:None,pending:None,message:String::new()}
    }
}
impl IdeApp {
    fn script_authority(&self) -> Option<Authority> {
        let project = self.project.as_ref()?;
        let configured = self.settings.plugins.scripting_interpreter.trim();
        let interpreter = if configured.is_empty() {
            project.interpreter.as_ref()?.path.clone()
        } else {
            PathBuf::from(configured)
        };
        Some(Authority {
            root: project.root.clone(),
            workspace: self.file_session,
            interpreter,
            source: self.plugin_runtime.script.source.clone(),
        })
    }
    fn script_authorized(&self) -> bool {
        self.plugin_runtime.script.consent
            && !self.project_safe_mode()
            && self.script_authority().is_some_and(|scope| {
                self.plugin_runtime.script.consent_scope.as_ref() == Some(&scope)
            })
    }

    pub(super) fn plugin_script_jobs(&self) -> Vec<(u64, String)> {
        self.plugin_runtime
            .script
            .pending
            .iter()
            .map(|pending| (pending.task.id() | SCRIPT_JOB, "IDE Python script".into()))
            .collect()
    }
    pub(super) fn stop_plugin_script(&mut self, id: u64) -> bool {
        if let Some(pending) = self
            .plugin_runtime
            .script
            .pending
            .as_mut()
            .filter(|pending| pending.task.id() | SCRIPT_JOB == id)
        {
            pending.stopping = true;
            pending.task.cancel();
            true
        } else {
            false
        }
    }
    pub(super) fn stop_all_plugin_scripts(&mut self) {
        self.plugin_runtime.script.consent = false;
        self.plugin_runtime.script.consent_scope = None;
        if let Some(pending) = self.plugin_runtime.script.pending.as_mut() {
            pending.stopping = true;
            pending.task.cancel();
        }
    }
    pub(super) fn retire_plugin_script(&mut self, ctx: &egui::Context) -> bool {
        self.stop_all_plugin_scripts();
        if let Some(pending) = self.plugin_runtime.script.pending.as_mut() {
            if pending.task.try_result().is_none() {
                ctx.request_repaint_after(Duration::from_millis(25));
                return true;
            }
        }
        self.plugin_runtime.script.pending = None;
        false
    }
    pub(super) fn run_ide_script(&mut self, action: Option<String>) {
        if !self.script_authorized() || self.plugin_runtime.script.pending.is_some() {
            return;
        }
        let Some(project) = self.project.as_ref() else {
            return;
        };
        let root = project.root.clone();
        let python = if self
            .settings
            .plugins
            .scripting_interpreter
            .trim()
            .is_empty()
        {
            project
                .interpreter
                .as_ref()
                .map(|interpreter| interpreter.path.clone())
        } else {
            Some(PathBuf::from(
                self.settings.plugins.scripting_interpreter.trim(),
            ))
        };
        let Some(python) = python else {
            self.plugin_runtime.script.message = "Select a Python interpreter first".into();
            return;
        };
        if self.plugin_runtime.script.source.len() > 64 * 1024 {
            self.plugin_runtime.script.message = "IDE scripts are limited to 64 KiB".into();
            return;
        }
        let document = self
            .active
            .and_then(|index| self.tabs.get(index))
            .filter(|tab| usize::from(tab.state.doc.len()) <= 512 * 1024)
            .map(|tab| runtime::Document {
                path: tab
                    .state
                    .path
                    .as_ref()
                    .map(|path| path.display().to_string())
                    .unwrap_or_default(),
                source: tab.state.doc.text(),
                version: tab.state.doc.version(),
                offset: usize::from(tab.state.primary().head()),
            });
        let captured = document.as_ref().and_then(|document| {
            self.active
                .and_then(|index| self.tabs.get(index))
                .map(|tab| (tab.state.doc.identity(), document.version, document.offset))
        });
        let request = Request {
            api_version: 1,
            event: if action.is_some() { "action" } else { "script" }.into(),
            action,
            document,
        };
        let Some(authority) = self.script_authority() else {
            return;
        };
        let result = (|| -> Result<_, String> {
            let directory = Settings::config_dir().join("plugin-scripts");
            std::fs::create_dir_all(&directory).map_err(|e| e.to_string())?;
            let mut input =
                tempfile::NamedTempFile::new_in(directory).map_err(|e| e.to_string())?;
            serde_json::to_writer(
                &mut input,
                &serde_json::json!({"script":self.plugin_runtime.script.source,"request":request}),
            )
            .map_err(|e| e.to_string())?;
            input.flush().map_err(|e| e.to_string())?;
            let source_request = request.clone();
            let worker_root = root.clone();
            let root_identity = rustpy_settings::trust::DirectoryIdentity::capture(&root)
                .map_err(|error| error.to_string())?;
            let task = rustpy_core::TaskManager::global()
                .task("IDE Python script")
                .spawn(move |progress| {
                    if !root_identity.matches(&worker_root) {
                        return Err(rustpy_core::TaskError::Failed(
                            "IDE script project directory changed".into(),
                        ));
                    }
                    let mut command = rustpy_core::interpreter_launch::interpreter_command(&python);
                    command
                        .args(["-I", "-S", "-c", include_str!("plugin_scripting.py")])
                        .arg(input.path())
                        .current_dir(worker_root)
                        .env_clear();
                    let (stdout, stderr) = supervised_script(
                        &mut command,
                        progress.token(),
                        Duration::from_secs(10),
                        runtime::MAX_MESSAGE as u64,
                    )
                    .map_err(|error| rustpy_core::TaskError::Failed(error.to_string()))?;
                    let response: Response = serde_json::from_slice(&stdout).map_err(|error| {
                        rustpy_core::TaskError::Failed(format!(
                            "Invalid IDE script response: {error}; {}",
                            String::from_utf8_lossy(&stderr)
                        ))
                    })?;
                    let manifest = RuntimeManifest {
                        api_version: 1,
                        module: "script.wasm".into(),
                        capabilities: [
                            Capability::DocumentRead,
                            Capability::DocumentWrite,
                            Capability::Actions,
                            Capability::ToolWindows,
                            Capability::Inspections,
                            Capability::Completion,
                        ]
                        .into_iter()
                        .collect::<BTreeSet<_>>(),
                    };
                    runtime::validate_response(&response, &manifest, &source_request)
                        .map_err(rustpy_core::TaskError::Failed)?;
                    Ok(response)
                });
            Ok(Pending {
                task,
                request,
                root: root.clone(),
                workspace: self.file_session,
                document: captured,
                stopping: false,
                authority,
            })
        })();
        match result {
            Ok(pending) => {
                self.plugin_runtime.script.pending = Some(pending);
                self.plugin_runtime.script.message = "Running under a 10-second deadline…".into();
            }
            Err(error) => self.plugin_runtime.script.message = error,
        }
    }
    pub(super) fn poll_plugin_script(&mut self, ctx: &egui::Context) {
        let admitted = self.script_authorized();
        let authority = self.script_authority();
        if !admitted {
            self.plugin_runtime.script.consent = false;
            self.plugin_runtime.script.consent_scope = None;
            self.remove_script_results();
        }
        let Some(pending) = self.plugin_runtime.script.pending.as_mut() else {
            return;
        };
        let current = admitted
            && authority.as_ref() == Some(&pending.authority)
            && pending.workspace == self.file_session
            && self
                .project
                .as_ref()
                .is_some_and(|project| project.root == pending.root);
        if !current {
            pending.stopping = true;
            pending.task.cancel();
        }
        let Some(result) = pending.task.try_result() else {
            ctx.request_repaint_after(Duration::from_millis(25));
            return;
        };
        let pending = self.plugin_runtime.script.pending.take().unwrap();
        if !current || pending.stopping || pending.task.is_cancelled() {
            self.plugin_runtime.script.message =
                "IDE script cancelled; its editor results were discarded".into();
            return;
        }
        match result {
            Ok(response) => {
                self.plugin_runtime.script.message = response.status.clone();
                self.publish_plugin_response(
                    "ide-script",
                    &pending.request.event,
                    pending.document,
                    response,
                );
            }
            Err(error) => self.plugin_runtime.script.message = error.to_string(),
        }
    }
    pub(super) fn plugin_scripting_window(&mut self, ctx: &egui::Context) {
        let mut open = self.plugin_runtime.scripting;
        if !open {
            return;
        }
        egui::Window::new("IDE Scripting Console").open(&mut open).default_width(700.).show_modeless(ctx,|ui|{
            ui.colored_label(self.theme.warning,"Python scripts run with your OS permissions. They are supervised and cancellable, not sandboxed. WASM plugins use the separate isolated host.");
            ui.horizontal(|ui|{ui.label("Python executable (empty: selected SDK)");if ui.text_edit_singleline(&mut self.settings.plugins.scripting_interpreter).changed(){self.plugin_runtime.script.consent=false;self.stop_all_plugin_scripts();self.remove_script_results();let _=self.settings.save();}});
            if ui.add(egui::TextEdit::multiline(&mut self.plugin_runtime.script.source).code_editor().desired_rows(14)).changed(){self.plugin_runtime.script.consent=false;self.stop_all_plugin_scripts();self.remove_script_results();}
            ui.label("API: ide.document (path/source/version/offset), ide.notify(text), ide.replace(start,end,text), ide.inspect(...), ide.complete(...), ide.register_action(id,title), ide.tool_window(id,title,text,actions). Offsets are UTF-8 bytes. Action callbacks rerun this script with ide.event / ide.action.");
            let mut authorized = self.script_authorized();
            if ui.checkbox(&mut authorized,"I authorize this script to run with my OS permissions").changed() {
                self.stop_all_plugin_scripts();
                self.remove_script_results();
                if authorized && !self.project_safe_mode() {
                    self.plugin_runtime.script.consent_scope = self.script_authority();
                    self.plugin_runtime.script.consent = self.plugin_runtime.script.consent_scope.is_some();
                }
            }
            if ui.add_enabled(!self.project_safe_mode()&&self.plugin_runtime.script.consent&&self.plugin_runtime.script.pending.is_none(),egui::Button::new("Run script")).clicked(){self.run_ide_script(None);}
            if let Some(id)=self.plugin_script_jobs().first().map(|(id,_)|*id){if ui.button("Stop script").clicked(){self.stop_plugin_script(id);}}
            ui.label(&self.plugin_runtime.script.message);
        });
        self.plugin_runtime.scripting = open;
    }
}

/// Reader handles stay in this owner until the process tree is reaped and both
/// streams reach EOF. Capped collection keeps draining after overflow to avoid
/// blocking a child while the supervising worker terminates its whole tree.
struct ScriptProcess {
    child: std::process::Child,
    owner: rustpy_run::process_owner::ProcessOwner,
    readers: Vec<std::thread::JoinHandle<std::io::Result<Vec<u8>>>>,
}
impl Drop for ScriptProcess {
    fn drop(&mut self) {
        self.owner.stop(&mut self.child);
        while self.owner.poll(&mut self.child).ok().flatten().is_none() {
            std::thread::sleep(Duration::from_millis(10));
        }
        for reader in self.readers.drain(..) {
            let _ = reader.join();
        }
    }
}
fn script_reader(
    mut stream: impl std::io::Read,
    limit: usize,
    overflow: &std::sync::atomic::AtomicBool,
) -> std::io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    let mut buffer = [0; 8192];
    loop {
        let count = match stream.read(&mut buffer) {
            Ok(0) => return Ok(bytes),
            Ok(count) => count,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        };
        let retain = count.min(limit.saturating_sub(bytes.len()));
        bytes.extend_from_slice(&buffer[..retain]);
        if retain < count {
            overflow.store(true, std::sync::atomic::Ordering::Release);
        }
    }
}
fn supervised_script(
    command: &mut std::process::Command,
    cancel: &rustpy_core::CancellationToken,
    timeout: Duration,
    limit: u64,
) -> Result<(Vec<u8>, Vec<u8>), String> {
    use std::{
        process::Stdio,
        sync::{
            atomic::{AtomicBool, Ordering},
            Arc,
        },
        time::Instant,
    };
    cancel.check().map_err(|e| e.to_string())?;
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    rustpy_run::process_owner::ProcessOwner::prepare(command);
    let mut child = command.spawn().map_err(|e| e.to_string())?;
    let owner = match rustpy_run::process_owner::ProcessOwner::attach(&mut child) {
        Ok(owner) => owner,
        Err(error) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(error.to_string());
        }
    };
    let mut process = ScriptProcess {
        child,
        owner,
        readers: Vec::new(),
    };
    let overflow = Arc::new(AtomicBool::new(false));
    let stdout = process.child.stdout.take().ok_or("Script stdout missing")?;
    let stderr = process.child.stderr.take().ok_or("Script stderr missing")?;
    let flag = overflow.clone();
    process.readers.push(
        std::thread::Builder::new()
            .name("ide-script-stdout".into())
            .spawn(move || script_reader(stdout, limit as usize, &flag))
            .map_err(|e| e.to_string())?,
    );
    let flag = overflow.clone();
    process.readers.push(
        std::thread::Builder::new()
            .name("ide-script-stderr".into())
            .spawn(move || script_reader(stderr, limit as usize, &flag))
            .map_err(|e| e.to_string())?,
    );
    let started = Instant::now();
    let status = loop {
        cancel.check().map_err(|e| e.to_string())?;
        if started.elapsed() >= timeout {
            return Err("IDE script deadline exceeded".into());
        }
        if overflow.load(Ordering::Acquire) {
            return Err("IDE script output limit exceeded".into());
        }
        if let Some(status) = process
            .owner
            .poll(&mut process.child)
            .map_err(|e| e.to_string())?
        {
            break status;
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let mut outputs = Vec::new();
    while !process.readers.is_empty() {
        let reader = process.readers.remove(0);
        outputs.push(
            reader
                .join()
                .map_err(|_| "IDE script reader panicked")?
                .map_err(|e| e.to_string())?,
        );
    }
    if overflow.load(Ordering::Acquire) {
        return Err("IDE script output limit exceeded".into());
    }
    if !status.success() {
        return Err(format!("IDE script failed: {status}"));
    }
    let stderr = outputs.pop().ok_or("Script stderr missing")?;
    let stdout = outputs.pop().ok_or("Script stdout missing")?;
    Ok((stdout, stderr))
}

#[cfg(test)]
mod admission_tests {
    use super::*;
    #[test]
    fn consent_is_bound_to_exact_source_sdk_and_workspace() {
        let admitted = Authority {
            root: "/project".into(),
            workspace: 1,
            interpreter: "/sdk/python".into(),
            source: "print(1)".into(),
        };
        let mut changed = admitted.clone();
        changed.source.push(' ');
        assert!(changed != admitted);
        let mut changed = admitted.clone();
        changed.interpreter = "/other/python".into();
        assert!(changed != admitted);
        let mut changed = admitted.clone();
        changed.workspace += 1;
        assert!(changed != admitted);
        let mut changed = admitted.clone();
        changed.root = "/other".into();
        assert!(changed != admitted);
    }
    #[test]
    fn overflow_is_bounded_without_abandoning_pipe_drain() {
        let overflow = std::sync::atomic::AtomicBool::new(false);
        let mut source = std::io::Cursor::new(vec![b'x'; 65536]);
        let bytes = script_reader(&mut source, 32, &overflow).unwrap();
        assert_eq!(bytes.len(), 32);
        assert_eq!(source.position(), 65536);
        assert!(overflow.load(std::sync::atomic::Ordering::Acquire));
    }
    // These fixtures launch only this test executable when the deferred suite runs.
    #[test]
    fn script_process_fixture() {
        let Ok(mode) = std::env::var("RUSTPY_SCRIPT_PROCESS_FIXTURE") else {
            return;
        };
        if mode == "descendant" {
            std::fs::write(
                std::env::var_os("RUSTPY_SCRIPT_PID").unwrap(),
                std::process::id().to_string(),
            )
            .unwrap();
            std::thread::sleep(Duration::from_secs(30));
            return;
        }
        if mode == "timeout" {
            std::thread::sleep(Duration::from_secs(30));
            return;
        }
        if mode == "pipes" {
            let err = std::thread::spawn(|| {
                std::io::stderr()
                    .write_all(&vec![b'e'; 512 * 1024])
                    .unwrap();
            });
            std::io::stdout()
                .write_all(&vec![b'o'; 512 * 1024])
                .unwrap();
            err.join().unwrap();
            return;
        }
        let mut child = fixture_command("descendant").spawn().unwrap();
        let marker = std::env::var_os("RUSTPY_SCRIPT_PID").unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !std::path::Path::new(&marker).exists() {
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(10));
        }
        // Intentionally leave inherited output pipes open in a live descendant.
        let _ = child.try_wait();
    }
    fn fixture_command(mode: &str) -> std::process::Command {
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "app::plugin_scripting::admission_tests::script_process_fixture",
                "--nocapture",
            ])
            .env("RUSTPY_SCRIPT_PROCESS_FIXTURE", mode);
        command
    }
    #[test]
    fn supervised_scripts_bound_both_pipes_and_retire_on_timeout() {
        let token = rustpy_core::CancellationToken::new();
        assert!(supervised_script(
            &mut fixture_command("pipes"),
            &token,
            Duration::from_secs(5),
            1024
        )
        .is_err());
        assert!(supervised_script(
            &mut fixture_command("timeout"),
            &token,
            Duration::from_millis(100),
            1024
        )
        .is_err());
        token.cancel();
        assert!(supervised_script(
            &mut fixture_command("timeout"),
            &token,
            Duration::from_secs(5),
            1024
        )
        .is_err());
    }
    #[cfg(target_os = "linux")]
    #[test]
    fn leader_exit_retires_pipe_holding_descendant_before_returning() {
        let directory = tempfile::tempdir().unwrap();
        let marker = directory.path().join("descendant.pid");
        let mut command = fixture_command("parent");
        command.env("RUSTPY_SCRIPT_PID", &marker);
        supervised_script(
            &mut command,
            &rustpy_core::CancellationToken::new(),
            Duration::from_secs(8),
            65536,
        )
        .unwrap();
        let pid = std::fs::read_to_string(marker).unwrap();
        let status = std::fs::read_to_string(format!("/proc/{}/status", pid.trim()));
        assert!(
            status.is_err()
                || status
                    .unwrap()
                    .lines()
                    .any(|line| line.starts_with("State:") && line.contains('Z'))
        );
    }
}

#[cfg(test)]
mod configured_execution_tests {
    use super::*;
    use std::time::Instant;

    fn app(root: &Path) -> IdeApp {
        assert_eq!(
            std::env::var("RUSTPY_REQUIRE_PROVIDER_TESTS").as_deref(),
            Ok("1")
        );
        let python = PathBuf::from(
            std::env::var_os("RUSTPY_PROVIDER_PYTHON")
                .expect("explicit absolute real Python SDK required"),
        );
        assert!(python.is_absolute() && python.is_file());
        let mut settings = Settings::default();
        settings.trust_project(root);
        settings.plugins.scripting_interpreter = python.display().to_string();
        let mut app = IdeApp::new(settings, Some(root.into()));
        let mut state = EditorState::from_text("x", Language::PlainText);
        state.path = Some(root.join("actual-script.txt"));
        app.push_tab(state);
        app
    }
    fn admit(app: &mut IdeApp, source: String) {
        app.plugin_runtime.script.source = source;
        app.plugin_runtime.script.consent = true;
        app.plugin_runtime.script.consent_scope = app.script_authority();
        assert!(app.script_authorized());
    }
    fn finish(app: &mut IdeApp, ctx: &egui::Context) {
        let deadline = Instant::now() + Duration::from_secs(12);
        while app.plugin_runtime.script.pending.is_some() {
            app.poll_plugin_script(ctx);
            if Instant::now() >= deadline {
                app.stop_all_plugin_scripts();
                panic!(
                    "real selected Python did not retire: {}",
                    app.plugin_runtime.script.message
                );
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(app.plugin_script_jobs().is_empty());
    }
    #[cfg(target_os = "linux")]
    fn wait_file(app: &mut IdeApp, path: &Path) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !path.is_file() {
            assert!(
                app.plugin_runtime.script.pending.is_some(),
                "actual script was not admitted"
            );
            if Instant::now() >= deadline {
                app.stop_all_plugin_scripts();
                panic!(
                    "actual selected Python did not publish readiness: {}",
                    path.display()
                );
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    #[cfg(target_os = "linux")]
    fn retired(path: &Path) {
        let owners: Vec<u32> = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        for pid in owners {
            assert!(
                !PathBuf::from(format!("/proc/{pid}")).exists(),
                "owned script/descendant {pid} survived physical retirement"
            );
        }
    }

    #[test]
    #[ignore = "explicit real RUSTPY_PROVIDER_PYTHON and serial isolated App config required"]
    fn configured_actual_python_ide_api_publishes_actions_diagnostics_completion_and_undo() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        let mut app = app(&root);
        let index = app.active.unwrap();
        let ctx = egui::Context::default();
        let source = r#"import json, os
with open(os.path.join(os.path.dirname(ide.document['path']), 'positive-owners.json'), 'w') as stream:
    json.dump([os.getpid()], stream)
ide.notify('Actual Python IDE API publication')
ide.register_action('actual.replace', 'Actual Python replace')
ide.tool_window('actual.window', 'Actual Python window', 'Actual Python window body', ['actual.replace'])
ide.inspect(0, 1, 'Actual Python diagnostic', 'ActualPython')
ide.complete('actual_python_value', 'actual_python_value', 'Actual Python completion')
if ide.action == 'actual.replace':
    ide.replace(0, 1, 'y')
print('Actual Python transcript π')
"#.to_string();
        admit(&mut app, source);
        app.run_ide_script(None);
        assert!(app.plugin_runtime.script.pending.is_some());
        finish(&mut app, &ctx);
        assert!(app
            .plugin_runtime
            .script
            .message
            .contains("Actual Python IDE API publication"));
        assert!(app
            .plugin_runtime
            .script
            .message
            .contains("Actual Python transcript π"));
        assert_eq!(app.tabs[index].state.doc.text(), "x");
        assert!(app.tabs[index]
            .state
            .plugin_diagnostics
            .as_ref()
            .unwrap()
            .1
            .iter()
            .any(|diagnostic| diagnostic.message.contains("Actual Python diagnostic")));
        assert!(app.open_plugin_completion(true));
        assert!(app
            .completion
            .items
            .iter()
            .any(|item| item.label.as_str() == "actual_python_value"));
        let mut painted = Vec::new();
        for frame in 0..4 {
            // A newly opened egui window first measures its content. Give it
            // bounded real frames before requiring the painted body text.
            let output = ctx.run_ui(
                egui::RawInput {
                    time: Some(f64::from(frame) / 60.0),
                    focused: true,
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(1200.0, 800.0),
                    )),
                    ..Default::default()
                },
                |ui| app.plugin_runtime_windows(ui.ctx()),
            );
            painted.extend(output.shapes.iter().filter_map(|shape| match &shape.shape {
                egui::Shape::Text(text) => Some(text.galley.text().to_owned()),
                _ => None,
            }));
            // Dispose before assertions, including their failure/unwind path.
            output.drop_without_applying_deltas();
            if painted
                .iter()
                .any(|text| text.contains("Actual Python window body"))
            {
                break;
            }
        }
        assert!(
            painted
                .iter()
                .any(|text| text.contains("Actual Python window body")),
            "actual window body was not painted after sizing frames: {painted:?}"
        );
        #[cfg(target_os = "linux")]
        retired(&root.join("positive-owners.json"));
        app.queue_plugin_action("ide-script", "actual.replace");
        assert!(app.plugin_runtime.script.pending.is_some());
        finish(&mut app, &ctx);
        assert_eq!(app.tabs[index].state.doc.text(), "y");
        assert!(app.tabs[index].state.undo());
        assert_eq!(app.tabs[index].state.doc.text(), "x");
        #[cfg(target_os = "linux")]
        retired(&root.join("positive-owners.json"));
        app.plugin_runtime
            .script
            .source
            .push_str("\n# changed after admission\n");
        app.run_ide_script(None);
        assert!(
            app.plugin_runtime.script.pending.is_none(),
            "changed source needs a new exact consent"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "explicit real Python SDK; actual stale publication, revocation and owned descendant retirement"]
    fn configured_actual_python_stale_result_and_revocation_retire_physical_owners() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        let mut app = app(&root);
        let index = app.active.unwrap();
        let ctx = egui::Context::default();
        // Complete a real response without polling App publication, then change
        // the editor. The captured document epoch must reject that response.
        admit(
            &mut app,
            r#"import json, os
ide.replace(0, 1, 'STALE')
with open(os.path.join(os.path.dirname(ide.document['path']), 'stale-ready'), 'w') as stream:
    stream.write('ready')
"#
            .into(),
        );
        app.run_ide_script(None);
        wait_file(&mut app, &root.join("stale-ready"));
        app.tabs[index].state.apply_text_edits(
            vec![(
                TextRange::new(TextSize::new(0), TextSize::new(1)),
                "newer".into(),
            )],
            "newer editor",
        );
        finish(&mut app, &ctx);
        assert_eq!(app.tabs[index].state.doc.text(), "newer");

        admit(
            &mut app,
            r#"import json, os, subprocess, sys, time
child = subprocess.Popen([sys.executable, '-I', '-S', '-c', 'import time; time.sleep(60)'])
path = os.path.join(os.path.dirname(ide.document['path']), 'waiting-owners.json')
with open(path + '.tmp', 'w') as stream:
    json.dump([os.getpid(), child.pid], stream)
    stream.flush()
    os.fsync(stream.fileno())
os.rename(path + '.tmp', path)
time.sleep(60)
ide.replace(0, 5, 'UNAUTHORIZED')
"#
            .into(),
        );
        app.run_ide_script(None);
        wait_file(&mut app, &root.join("waiting-owners.json"));
        app.plugin_runtime.script.consent = false;
        finish(&mut app, &ctx);
        assert_eq!(app.tabs[index].state.doc.text(), "newer");
        assert!(app.plugin_runtime.script.message.contains("cancelled"));
        retired(&root.join("waiting-owners.json"));
        assert!(!app.retire_plugin_script(&ctx));
    }
}

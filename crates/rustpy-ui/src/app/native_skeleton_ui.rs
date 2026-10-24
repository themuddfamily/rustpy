//! Explicit trusted action; all interpreter execution belongs to a managed task.
use super::*;
use rustpy_core::{CancellationToken, TaskHandle, TaskManager};
use rustpy_semantic::native_skeletons::{Catalogue, InterpreterBinding, Record};
use rustpy_semantic::native_state::{NativeState, VerifiedNativeIndex};
use std::collections::BTreeMap;

#[derive(Clone)]
struct Capture {
    project: PathBuf,
    analysis: Arc<Analysis>,
    interpreter: PathBuf,
    version: String,
    library_root: PathBuf,
    module_path: PathBuf,
    module_name: String,
    selection: Option<PathBuf>,
    active_path: Option<PathBuf>,
}
struct Publication {
    state: NativeState,
    records: Vec<Record>,
    identities: Vec<(PathBuf, FileIdentity)>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct FileIdentity {
    len: u64,
    modified: std::time::SystemTime,
    #[cfg(unix)]
    unix: (u64, u64, i64, i64),
    #[cfg(windows)]
    windows: (u64, u64, u32),
}
impl FileIdentity {
    fn read(path: &Path) -> std::io::Result<Self> {
        let metadata = std::fs::symlink_metadata(path)?;
        if !metadata.is_file() {
            return Err(std::io::Error::other(
                "Native binding is no longer a regular file",
            ));
        }
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt;
        #[cfg(windows)]
        use std::os::windows::fs::MetadataExt;
        Ok(Self {
            len: metadata.len(),
            modified: metadata.modified()?,
            #[cfg(unix)]
            unix: (
                metadata.dev(),
                metadata.ino(),
                metadata.ctime(),
                metadata.ctime_nsec(),
            ),
            #[cfg(windows)]
            windows: (
                metadata.creation_time(),
                metadata.last_write_time(),
                metadata.file_attributes(),
            ),
        })
    }
}
struct Job {
    manager: TaskManager,
    handle: TaskHandle<Publication>,
    capture: Capture,
    result: Option<Result<Publication, String>>,
    generation: u64,
    validated: bool,
}
impl Job {
    fn poll(&mut self) {
        if self.result.is_none() {
            self.result = self
                .handle
                .try_result()
                .map(|value| value.map_err(|e| e.to_string()));
        }
    }
    fn pending(&self) -> bool {
        self.result.is_none() || self.manager.is_running(self.handle.id())
    }
}
#[derive(Default)]
pub(super) struct NativeSkeletonUi {
    domain: CancellationToken,
    generation: u64,
    active: Option<Job>,
    draining: Vec<Job>,
    records: Vec<Record>,
}
impl NativeSkeletonUi {
    /// Already published, trusted catalogue only; browsing never starts a target process.
    pub(super) fn published_module_names(&self) -> Vec<String> {
        self.records
            .iter()
            .map(|record| record.module_name.clone())
            .collect()
    }

    fn cancel(&mut self, revoke: bool) {
        self.domain.cancel();
        if let Some(job) = self.active.take() {
            self.draining.push(job);
        }
        for job in &self.draining {
            job.handle.cancel();
        }
        if revoke {
            self.records.clear();
        }
        self.domain = CancellationToken::new();
        self.generation = self.generation.saturating_add(1);
    }
    fn pending(&self) -> bool {
        self.active.as_ref().is_some_and(Job::pending) || self.draining.iter().any(Job::pending)
    }
    fn stamps(&self) -> Vec<String> {
        let mut stamps: Vec<_> = self
            .active
            .iter()
            .chain(self.draining.iter())
            .map(|job| {
                format!(
                    "native-skeleton:{}:{}:{}",
                    job.handle.id(),
                    job.manager.is_running(job.handle.id()),
                    job.handle.is_cancelled()
                )
            })
            .collect();
        stamps.sort();
        stamps
    }
    fn start(&mut self, capture: Capture) -> Result<(), &'static str> {
        if self.pending() {
            return Err("Stub generation is already running");
        }
        self.generation = self
            .generation
            .checked_add(1)
            .ok_or("Stub generation counter exhausted")?;
        let generation = self.generation;
        let previous = self.records.clone();
        let worker_capture = capture.clone();
        let manager = TaskManager::global().clone();
        let handle = manager
            .task("Generate native module stub")
            .child_of(&self.domain)
            .spawn(move |progress| {
                let worker_started = std::time::Instant::now();
                let artifact = rustpy_project::native_skeleton::prepare_and_produce(
                    &worker_capture.interpreter,
                    &worker_capture.version,
                    &worker_capture.module_path,
                    &worker_capture.module_name,
                    &worker_capture.library_root,
                    progress.token(),
                )?;
                progress.token().check()?;
                if artifact.interpreter != worker_capture.interpreter
                    || artifact.version != worker_capture.version
                    || artifact.module_path != worker_capture.module_path
                    || artifact.module_name != worker_capture.module_name
                    || artifact.library_root != worker_capture.library_root
                {
                    return Err(rustpy_core::TaskError::Failed(
                        "Generated artifact does not match captured request".into(),
                    ));
                }
                let interpreter = InterpreterBinding {
                    path: artifact.interpreter,
                    sha256: artifact.interpreter_sha256,
                    version: artifact.version,
                    cache_tag: artifact.cache_tag,
                    soabi: artifact.soabi,
                };
                let mut records = Vec::new();
                for record in previous {
                    progress.token().check()?;
                    if worker_started.elapsed() > std::time::Duration::from_secs(30) {
                        return Err(rustpy_core::TaskError::Failed(
                            "Skeleton publication verification timed out".into(),
                        ));
                    }
                    if record.interpreter == interpreter
                        && record.module_path != artifact.module_path
                        && rustpy_project::native_skeleton::fingerprint_file(
                            &record.module_path,
                            progress.token(),
                        )
                        .is_ok_and(|digest| digest == record.module_sha256)
                    {
                        records.push(record);
                    }
                }
                records.push(Record {
                    protocol: artifact.protocol,
                    module_name: artifact.module_name,
                    module_path: artifact.module_path,
                    module_sha256: artifact.module_sha256,
                    interpreter: interpreter.clone(),
                    pyi: artifact.pyi,
                    unsupported: artifact.unsupported,
                });
                let modules: BTreeMap<_, _> = records
                    .iter()
                    .map(|record| {
                        (
                            (record.module_name.clone(), record.module_path.clone()),
                            record.module_sha256.clone(),
                        )
                    })
                    .collect();
                let catalogue =
                    Catalogue::from_records(true, generation, interpreter.clone(), records.clone())
                        .map_err(rustpy_core::TaskError::from)?;
                progress.token().check()?;
                Ok(Publication {
                    state: NativeState {
                        catalogue: Arc::new(catalogue),
                        index: VerifiedNativeIndex {
                            trusted: true,
                            generation,
                            interpreter: Some(interpreter),
                            modules,
                        },
                    },
                    records,
                    identities: Vec::new(),
                })
            });
        self.active = Some(Job {
            manager,
            handle,
            capture,
            result: None,
            generation,
            validated: false,
        });
        Ok(())
    }
    fn revalidate(
        &mut self,
        capture: Capture,
        publication: Publication,
    ) -> Result<(), &'static str> {
        self.generation = self
            .generation
            .checked_add(1)
            .ok_or("Stub generation counter exhausted")?;
        let generation = self.generation;
        let manager = TaskManager::global().clone();
        let handle = manager
            .task("Verify native stub publication")
            .child_of(&self.domain)
            .spawn(move |progress| verify_publication(publication, generation, progress.token()));
        self.active = Some(Job {
            manager,
            handle,
            capture,
            result: None,
            generation,
            validated: true,
        });
        Ok(())
    }
}

fn verify_publication(
    mut publication: Publication,
    generation: u64,
    token: &CancellationToken,
) -> Result<Publication, rustpy_core::TaskError> {
    let started = std::time::Instant::now();
    let fail = |text: &str| rustpy_core::TaskError::Failed(text.into());
    let interpreter = publication
        .state
        .index
        .interpreter
        .clone()
        .ok_or_else(|| fail("Missing interpreter binding"))?;
    let mut expected = BTreeMap::new();
    expected.insert(interpreter.path.clone(), interpreter.sha256.clone());
    for record in &publication.records {
        if record.interpreter != interpreter {
            return Err(fail("Mixed interpreter bindings"));
        }
        if expected
            .insert(record.module_path.clone(), record.module_sha256.clone())
            .is_some_and(|digest| digest != record.module_sha256)
        {
            return Err(fail("Conflicting native bindings"));
        }
    }
    let mut identities = Vec::new();
    let mut verified = BTreeMap::new();
    for (path, expected_digest) in expected {
        token.check()?;
        if started.elapsed() > std::time::Duration::from_secs(30) {
            return Err(fail("Native publication verification timed out"));
        }
        let before = FileIdentity::read(&path).map_err(|e| fail(&e.to_string()))?;
        let digest = rustpy_project::native_skeleton::fingerprint_file(&path, token)?;
        let after = FileIdentity::read(&path).map_err(|e| fail(&e.to_string()))?;
        if before != after || digest != expected_digest {
            return Err(fail("Native binding changed before publication"));
        }
        verified.insert(path.clone(), digest);
        identities.push((path, after));
    }
    // Recheck every binding after the whole batch, not just immediately after its own hash.
    for (path, identity) in &identities {
        token.check()?;
        if FileIdentity::read(path).as_ref().ok() != Some(identity) {
            return Err(fail(
                "Native binding changed during publication verification",
            ));
        }
    }
    if started.elapsed() > std::time::Duration::from_secs(30) {
        return Err(fail("Native publication verification timed out"));
    }
    let modules = publication
        .records
        .iter()
        .map(|record| {
            (
                (record.module_name.clone(), record.module_path.clone()),
                verified[&record.module_path].clone(),
            )
        })
        .collect();
    let catalogue = Catalogue::from_records(
        true,
        generation,
        interpreter.clone(),
        publication.records.clone(),
    )
    .map_err(rustpy_core::TaskError::from)?;
    publication.state = NativeState {
        catalogue: Arc::new(catalogue),
        index: VerifiedNativeIndex {
            trusted: true,
            generation,
            interpreter: Some(interpreter),
            modules,
        },
    };
    publication.identities = identities;
    token.check()?;
    Ok(publication)
}
impl IdeApp {
    fn capture_native_stub(&self) -> Result<Capture, &'static str> {
        let project_safe_mode = self.project_safe_mode();
        if project_safe_mode {
            return Err("Trust the project and close modal controls before generating a stub");
        }
        let project = self.project.as_ref().ok_or("Open a project first")?;
        let interpreter = project
            .interpreter
            .as_ref()
            .ok_or("Select a project interpreter first")?;
        if project.interpreter_rx.is_some() {
            return Err("Interpreter discovery is still running");
        }
        let active_path = self
            .active
            .and_then(|i| self.tabs.get(i))
            .and_then(|tab| tab.state.path.clone());
        let selection = project.tree.selected.clone();
        let target = selection
            .as_ref()
            .filter(|path| {
                // Preserve the explicitly selected native image for trust/library
                // admission before validating its importable module basename.
                path.extension()
                    .is_some_and(|extension| extension == "so" || extension == "pyd")
            })
            .or(active_path.as_ref())
            .ok_or("Select a native library module first")?;
        let module_path =
            dunce::canonicalize(target).map_err(|_| "Native module is unavailable")?;
        let library_root = interpreter
            .sys_path
            .iter()
            .filter_map(|root| dunce::canonicalize(root).ok())
            .filter(|root| root != &project.root && module_path.starts_with(root))
            .max_by_key(|root| root.components().count())
            .ok_or("The module must belong to the selected interpreter library")?;
        let relative = module_path
            .strip_prefix(&library_root)
            .map_err(|_| "Module outside interpreter library")?;
        let leaf = relative
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(rustpy_semantic::module::extension_module_name)
            .ok_or("Select a .so or .pyd extension")?;
        let mut parts: Vec<String> = relative
            .parent()
            .into_iter()
            .flat_map(|parent| parent.components())
            .map(|part| part.as_os_str().to_string_lossy().into_owned())
            .collect();
        if !parts
            .iter()
            .all(|part| rustpy_semantic::module::is_identifier(part))
        {
            return Err("Unsupported package path");
        }
        parts.push(leaf.into());
        Ok(Capture {
            project: project.root.clone(),
            analysis: project.analysis.clone(),
            interpreter: dunce::canonicalize(&interpreter.path)
                .map_err(|_| "Interpreter is unavailable")?,
            version: interpreter.version.clone(),
            library_root,
            module_path,
            module_name: parts.join("."),
            selection,
            active_path,
        })
    }
    pub(super) fn generate_native_stub(&mut self) {
        if !self.editor_controls_available() || self.coordinated_quit_active() {
            self.set_status("Close modal controls before generating a stub");
            return;
        }
        let result = self
            .capture_native_stub()
            .and_then(|capture| self.native_skeleton_ui.start(capture));
        self.set_status(match result {
            Ok(()) => "Generating native module stub…",
            Err(error) => error,
        });
    }
    fn native_capture_current(&self, capture: &Capture) -> bool {
        self.capture_native_stub().is_ok_and(|current| {
            current.project == capture.project
                && Arc::ptr_eq(&current.analysis, &capture.analysis)
                && current.interpreter == capture.interpreter
                && current.version == capture.version
                && current.module_path == capture.module_path
                && current.module_name == capture.module_name
                && current.library_root == capture.library_root
                && current.selection == capture.selection
                && current.active_path == capture.active_path
        })
    }
    pub(super) fn poll_native_skeletons(&mut self, ctx: &egui::Context) {
        let project_safe_mode = self.project_safe_mode();
        if project_safe_mode
            && (!self.native_skeleton_ui.records.is_empty()
                || self.native_skeleton_ui.active.is_some())
        {
            self.cancel_native_skeletons();
        }
        let obsolete = self
            .native_skeleton_ui
            .active
            .as_ref()
            .is_some_and(|job| !self.native_capture_current(&job.capture));
        if obsolete {
            let same_context = self.native_skeleton_ui.active.as_ref().is_some_and(|job| {
                !project_safe_mode
                    && self.project.as_ref().is_some_and(|project| {
                        project.root == job.capture.project
                            && Arc::ptr_eq(&project.analysis, &job.capture.analysis)
                            && project.interpreter.as_ref().is_some_and(|interpreter| {
                                dunce::canonicalize(&interpreter.path)
                                    .is_ok_and(|path| path == job.capture.interpreter)
                                    && interpreter.version == job.capture.version
                                    && interpreter.sys_path.iter().any(|root| {
                                        dunce::canonicalize(root)
                                            .is_ok_and(|root| root == job.capture.library_root)
                                    })
                            })
                    })
            });
            if same_context {
                // A different selection invalidates the in-flight result, not
                // previously verified stubs belonging to the same interpreter.
                self.native_skeleton_ui.cancel(false);
                self.set_status("Native stub generation cancelled because the target changed");
            } else {
                self.cancel_native_skeletons();
            }
        }
        for job in &mut self.native_skeleton_ui.draining {
            job.poll();
        }
        self.native_skeleton_ui.draining.retain(Job::pending);
        if let Some(job) = &mut self.native_skeleton_ui.active {
            job.poll();
        }
        let consent = self.coordinated_quit_active() || self.exit_questions_open();
        if consent {
            // A verification queued across consent is never publication authority.
            if let Some(job) = &mut self.native_skeleton_ui.active {
                job.validated = false;
            }
        }
        let ready = !consent
            && self
                .native_skeleton_ui
                .active
                .as_ref()
                .is_some_and(|job| !job.pending());
        if ready {
            let mut job = self.native_skeleton_ui.active.take().unwrap();
            if !job.handle.is_cancelled() && self.native_capture_current(&job.capture) {
                match job.result.take().unwrap() {
                    Ok(publication) => {
                        if job.generation != self.native_skeleton_ui.generation {
                            self.set_status("Native stub publication generation changed");
                        } else if !job.validated {
                            if let Err(error) =
                                self.native_skeleton_ui.revalidate(job.capture, publication)
                            {
                                self.set_status(error);
                            }
                        } else if publication.identities.iter().all(|(path, identity)| {
                            FileIdentity::read(path).as_ref().ok() == Some(identity)
                        }) {
                            job.capture.analysis.set_native_skeletons(publication.state);
                            self.native_skeleton_ui.records = publication.records;
                            self.set_status("Native module stub generated");
                        } else {
                            self.set_status("Native binding changed while publication was queued");
                        }
                    }
                    Err(error) => {
                        self.set_status(format!("Native stub generation failed: {error}"))
                    }
                }
            }
        }
        self.sync_external_file_watches();
        if self.native_skeletons_pending() {
            ctx.request_repaint_after(std::time::Duration::from_millis(16));
        }
    }
    pub(super) fn cancel_native_skeletons(&mut self) {
        self.native_skeleton_ui.cancel(true);
        if let Some(project) = &self.project {
            project
                .analysis
                .set_native_skeletons(NativeState::default());
        }
    }
    pub(super) fn native_skeletons_pending(&self) -> bool {
        self.native_skeleton_ui.pending()
    }
    pub(super) fn native_skeleton_paths(&self) -> Vec<PathBuf> {
        let mut paths: Vec<_> = self
            .native_skeleton_ui
            .records
            .iter()
            .flat_map(|record| [record.module_path.clone(), record.interpreter.path.clone()])
            .collect();
        paths.extend(
            self.native_skeleton_ui
                .active
                .iter()
                .chain(self.native_skeleton_ui.draining.iter())
                .flat_map(|job| {
                    [
                        job.capture.module_path.clone(),
                        job.capture.interpreter.clone(),
                    ]
                }),
        );
        paths.sort();
        paths.dedup();
        paths
    }
    pub(super) fn is_native_skeleton_event(&self, path: &Path) -> bool {
        let path = rustpy_core::vfs::normalize_path(path);
        self.native_skeleton_paths()
            .iter()
            .any(|native| native == &path || native.starts_with(&path))
    }
    pub(super) fn native_skeleton_stamps(&self) -> Vec<String> {
        self.native_skeleton_ui.stamps()
    }
}

impl Drop for NativeSkeletonUi {
    fn drop(&mut self) {
        self.domain.cancel();
        for job in self.active.iter().chain(self.draining.iter()) {
            job.handle.cancel();
        }
    }
}

#[cfg(all(test, unix))]
#[path = "native_skeleton_ui_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "native_skeleton_actual_success_tests.rs"]
mod actual_success_tests;

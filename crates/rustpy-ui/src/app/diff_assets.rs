//! Bounded asset worker and external-tool ownership, independent of dialog markers.
use super::*;
use crate::a11y::AccessibleCombo;
use crate::a11y::DialogWindow;
use crate::diff_view::{assets::Assets, SideBySide};
pub(super) struct AssetWindow {
    left: PathBuf,
    right: PathBuf,
    visible: bool,
    rx: Option<std::sync::mpsc::Receiver<Result<Assets, String>>>,
    value: Option<Assets>,
    error: Option<String>,
    textures: Vec<egui::TextureHandle>,
    cells: Vec<SideBySide>,
    selected: usize,
    overlay: bool,
    opacity: f32,
}

pub(super) struct ExternalJob {
    child: std::process::Child,
    _directory: Option<tempfile::TempDir>,
    output: Option<(PathBuf, (PathBuf, String, String, String))>,
}
pub(super) fn asset_path(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            matches!(
                extension.to_ascii_lowercase().as_str(),
                "ipynb"
                    | "png"
                    | "jpg"
                    | "jpeg"
                    | "gif"
                    | "webp"
                    | "bmp"
                    | "ico"
                    | "tiff"
                    | "bin"
                    | "dat"
                    | "exe"
                    | "dll"
                    | "so"
                    | "pdf"
                    | "zip"
            )
        })
}
impl IdeApp {
    pub(super) fn compare_paths_workspace(
        &mut self,
        left: &Path,
        right: &Path,
    ) -> Result<(), String> {
        if self.settings.diff_merge.external_files {
            if let (Ok(left_text), Ok(right_text)) = (
                self.capture_comparison(Some(left), false),
                self.capture_comparison(Some(right), false),
            ) {
                if !left_text.asset && !right_text.asset {
                    return self.external_text_diff(&left_text.text, &right_text.text);
                }
            }
            self.external_diff_paths(left, right, false)?;
            return Ok(());
        }
        if asset_path(left) || asset_path(right) {
            return self.open_asset_diff(left.into(), right.into());
        }
        match self.capture_comparison(Some(left), false) {
            Ok(source) => match self.compare_captured_file(&source, right) {
                Ok(()) => Ok(()),
                Err(_) => self.open_asset_diff(left.into(), right.into()),
            },
            Err(_) => self.open_asset_diff(left.into(), right.into()),
        }
    }
    pub(super) fn open_asset_diff(&mut self, left: PathBuf, right: PathBuf) -> Result<(), String> {
        if self
            .diff_workspace
            .assets
            .as_ref()
            .is_some_and(|state| state.rx.is_some())
        {
            return Err("An asset comparison is still reading; wait for it to finish".into());
        }
        let (a, b) = (left.clone(), right.clone());
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        std::thread::spawn(move || {
            let _ = tx.send(crate::diff_view::assets::read_pair(&a, &b));
        });
        self.diff_workspace.assets = Some(AssetWindow {
            left,
            right,
            visible: true,
            rx: Some(rx),
            value: None,
            error: None,
            textures: Vec::new(),
            cells: Vec::new(),
            selected: 0,
            overlay: false,
            opacity: 0.5,
        });
        self.dialog = Dialog::None;
        Ok(())
    }
    pub(super) fn external_diff_paths(
        &mut self,
        left: &Path,
        right: &Path,
        folder: bool,
    ) -> Result<(), String> {
        let project_safe_mode = self.project_safe_mode();
        if project_safe_mode {
            return Err("External diff tools require a trusted project".into());
        }
        let settings = &self.settings.diff_merge;
        let enabled = if folder {
            settings.external_folders
        } else {
            settings.external_files
        };
        if !enabled {
            return Err("External diff tool is disabled".into());
        }
        let program = settings.diff_program.clone();
        let template = settings.diff_arguments.clone();
        self.start_diff_external(&program, &template, left, right, None)
    }
    pub(super) fn external_text_diff(&mut self, left: &str, right: &str) -> Result<(), String> {
        let directory_root = dirs::cache_dir()
            .unwrap_or_else(std::env::temp_dir)
            .join("rustpy/diff-inputs");
        std::fs::create_dir_all(&directory_root).map_err(|error| error.to_string())?;
        let directory = tempfile::tempdir_in(directory_root).map_err(|error| error.to_string())?;
        let a = directory.path().join("left.txt");
        let b = directory.path().join("right.txt");
        std::fs::write(&a, left)
            .and_then(|_| std::fs::write(&b, right))
            .map_err(|error| error.to_string())?;
        let program = self.settings.diff_merge.diff_program.clone();
        let template = self.settings.diff_merge.diff_arguments.clone();
        self.start_diff_external(&program, &template, &a, &b, Some(directory))
    }
    fn start_diff_external(
        &mut self,
        program: &str,
        template: &str,
        left: &Path,
        right: &Path,
        directory: Option<tempfile::TempDir>,
    ) -> Result<(), String> {
        let project_safe_mode = self.project_safe_mode();
        if project_safe_mode {
            return Err("External diff tools require a trusted project".into());
        }
        if self.diff_workspace.external.is_some() {
            return Err("An external comparison is already running".into());
        }
        if program.trim().is_empty() {
            return Err("Configure an external diff program first".into());
        }
        let arguments: Vec<_> = crate::external_tools::split_template(template)
            .into_iter()
            .map(|(word, _)| {
                word.replace("$LEFT$", &left.to_string_lossy())
                    .replace("$RIGHT$", &right.to_string_lossy())
            })
            .collect();
        let child = std::process::Command::new(program)
            .args(arguments)
            .stdin(std::process::Stdio::null())
            .spawn()
            .map_err(|error| error.to_string())?;
        self.diff_workspace.external = Some(ExternalJob {
            child,
            _directory: directory,
            output: None,
        });
        Ok(())
    }
    pub(super) fn start_external_merge(
        &mut self,
        path: &Path,
        base: &str,
        ours: &str,
        theirs: &str,
    ) -> Result<(), String> {
        let project_safe_mode = self.project_safe_mode();
        if project_safe_mode {
            return Err("External merge requires a trusted project".into());
        }
        if self.diff_workspace.external.is_some() {
            return Err("An external tool is already running".into());
        }
        let root = dirs::cache_dir()
            .unwrap_or_else(std::env::temp_dir)
            .join("rustpy/diff-inputs");
        std::fs::create_dir_all(&root).map_err(|error| error.to_string())?;
        let directory = tempfile::tempdir_in(root).map_err(|error| error.to_string())?;
        let left = directory.path().join("theirs.txt");
        let ancestor = directory.path().join("base.txt");
        let right = directory.path().join("ours.txt");
        let output = directory.path().join("result.txt");
        for (file, text) in [
            (&left, theirs),
            (&ancestor, base),
            (&right, ours),
            (&output, ours),
        ] {
            std::fs::write(file, text).map_err(|error| error.to_string())?;
        }
        let args: Vec<_> =
            crate::external_tools::split_template(&self.settings.diff_merge.merge_arguments)
                .into_iter()
                .map(|(word, _)| {
                    word.replace("$LEFT$", &left.to_string_lossy())
                        .replace("$BASE$", &ancestor.to_string_lossy())
                        .replace("$RIGHT$", &right.to_string_lossy())
                        .replace("$OUTPUT$", &output.to_string_lossy())
                })
                .collect();
        let child = std::process::Command::new(&self.settings.diff_merge.merge_program)
            .args(args)
            .stdin(std::process::Stdio::null())
            .spawn()
            .map_err(|error| error.to_string())?;
        self.diff_workspace.external = Some(ExternalJob {
            child,
            _directory: Some(directory),
            output: Some((
                output,
                (path.into(), base.into(), ours.into(), theirs.into()),
            )),
        });
        Ok(())
    }
    pub(super) fn diff_asset_controls_owned(&self) -> bool {
        self.diff_workspace
            .assets
            .as_ref()
            .is_some_and(|state| state.visible)
    }
    pub(super) fn diff_workspace_jobs_pending(&self) -> bool {
        self.diff_workspace.external.is_some()
            || self
                .diff_workspace
                .assets
                .as_ref()
                .is_some_and(|state| state.rx.is_some())
    }
    pub(super) fn diff_workspace_job_stamp(&self) -> String {
        format!(
            "diff-worker:{:?}:{}",
            self.diff_workspace
                .external
                .as_ref()
                .map(|job| job.child.id()),
            self.diff_workspace
                .assets
                .as_ref()
                .is_some_and(|state| state.rx.is_some())
        )
    }
    pub(super) fn cancel_diff_workspace_jobs(&mut self) {
        if let Some(job) = &mut self.diff_workspace.external {
            let _ = job.child.kill();
        }
        if let Some(state) = &mut self.diff_workspace.assets {
            state.visible = false;
        }
    }
    pub(super) fn poll_diff_workspace_jobs(&mut self, ctx: &egui::Context) {
        if let Some(mut job) = self.diff_workspace.external.take() {
            match job.child.try_wait() {
                Ok(Some(status)) => {
                    if !status.success() && !(job.output.is_none() && status.code() == Some(1)) {
                        self.set_status(format!("External tool exited with {status}"));
                    } else if let Some((path, (owner, base, ours, theirs))) = &job.output {
                        let content = std::fs::read_to_string(path)
                            .ok()
                            .filter(|text| text.len() <= 2 * 1024 * 1024);
                        if let Some(content) = content {
                            if self.diff_workspace.merge.as_ref().is_some_and(
                                |(current, b, o, t, _)| {
                                    current == owner && b == base && o == ours && t == theirs
                                },
                            ) {
                                self.diff_workspace
                                    .merge
                                    .as_mut()
                                    .unwrap()
                                    .4
                                    .set_result(content);
                            } else {
                                self.diff_workspace.external_result = Some((
                                    (owner.clone(), base.clone(), ours.clone(), theirs.clone()),
                                    content,
                                ));
                                self.set_status(
                                    "External merge result retained for its original comparison",
                                );
                            }
                        } else {
                            self.set_status("External merge result is missing or exceeds 2 MiB");
                        }
                    }
                }
                Ok(None) => {
                    self.diff_workspace.external = Some(job);
                    ctx.request_repaint_after(std::time::Duration::from_millis(100));
                }
                Err(error) => self.set_status(error.to_string()),
            }
        }

        if let Some(state) = &mut self.diff_workspace.assets {
            if let Some(rx) = &state.rx {
                match rx.try_recv() {
                    Ok(result) => {
                        state.rx = None;
                        match result {
                            Ok(value) => state.value = Some(value),
                            Err(error) => state.error = Some(error),
                        }
                    }
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        state.rx = None;
                        state.error = Some("Asset comparison worker stopped".into());
                    }
                    _ => ctx.request_repaint_after(std::time::Duration::from_millis(40)),
                }
            }
        }
    }
    pub(super) fn diff_asset_windows(&mut self, ctx: &egui::Context) {
        self.poll_diff_workspace_jobs(ctx);
        let Some(mut state) = self.diff_workspace.assets.take() else {
            return;
        };
        if let Some(rx) = &state.rx {
            match rx.try_recv() {
                Ok(result) => {
                    state.rx = None;
                    match result {
                        Ok(value) => state.value = Some(value),
                        Err(error) => state.error = Some(error),
                    }
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    state.rx = None;
                    state.error = Some("Asset comparison worker stopped".into());
                }
                _ => {
                    ctx.request_repaint_after(std::time::Duration::from_millis(40));
                }
            }
        }
        if !state.visible {
            if state.rx.is_some() {
                self.diff_workspace.assets = Some(state);
            }
            return;
        }
        let mut open = true;
        egui::Window::new("Compare images / binary / notebook cells")
            .open(&mut open)
            .default_size([1000.0, 650.0])
            .show_modeless(ctx, |ui| {
                ui.label(format!(
                    "{} ↔ {}",
                    state.left.display(),
                    state.right.display()
                ));
                if let Some(error) = &state.error {
                    ui.label(error);
                }
                if state.rx.is_some() {
                    crate::a11y::spinner(ui);
                    ui.label("Reading bounded comparison operands…");
                }
                match &state.value {
                    Some(Assets::Binary { left, right }) => {
                        ui.columns(2, |columns| {
                            for (column, stats) in columns.iter_mut().zip([left, right]) {
                                column.label(format!("{} bytes", stats.size));
                                column.label(format!("SHA-256 {}", stats.sha256));
                            }
                        });
                        ui.label(
                            left.first_difference
                                .map(|offset| format!("First differing byte: {offset}"))
                                .unwrap_or_else(|| "Files are byte-identical".into()),
                        );
                    }
                    Some(Assets::Images {
                        left,
                        right,
                        hashes,
                    }) => {
                        if state.textures.is_empty() {
                            state.textures.push(ctx.load_texture(
                                "diff-image-left",
                                left.clone(),
                                egui::TextureOptions::LINEAR,
                            ));
                            state.textures.push(ctx.load_texture(
                                "diff-image-right",
                                right.clone(),
                                egui::TextureOptions::LINEAR,
                            ));
                        }
                        ui.label(format!(
                            "{}×{} / {}×{} pixels · {} / {} bytes",
                            left.size[0],
                            left.size[1],
                            right.size[0],
                            right.size[1],
                            hashes.0.size,
                            hashes.1.size
                        ));
                        ui.checkbox(&mut state.overlay, "Overlay images");
                        ui.add(
                            egui::Slider::new(&mut state.opacity, 0.0..=1.0)
                                .text("Overlay opacity"),
                        );
                        egui::ScrollArea::both().show(ui, |ui| {
                            if state.overlay {
                                let size = state.textures[0].size_vec2();
                                let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
                                let uv = egui::Rect::from_min_max(
                                    egui::Pos2::ZERO,
                                    egui::pos2(1.0, 1.0),
                                );
                                ui.painter().image(
                                    state.textures[0].id(),
                                    rect,
                                    uv,
                                    egui::Color32::WHITE,
                                );
                                ui.painter().image(
                                    state.textures[1].id(),
                                    rect,
                                    uv,
                                    egui::Color32::from_white_alpha((state.opacity * 255.0) as u8),
                                );
                            } else {
                                ui.horizontal(|ui| {
                                    for texture in &state.textures {
                                        ui.image((texture.id(), texture.size_vec2()));
                                    }
                                });
                            }
                        });
                    }
                    Some(Assets::Notebook(cells)) => {
                        if state.cells.is_empty() {
                            state.cells = cells
                                .iter()
                                .map(|cell| SideBySide::new(&cell.left, &cell.right))
                                .collect();
                        }
                        egui::ComboBox::from_id_salt("notebook-diff-cell")
                            .selected_text(
                                cells
                                    .get(state.selected)
                                    .map(|cell| cell.label.as_str())
                                    .unwrap_or("No cells"),
                            )
                            .show_ui(ui, |ui| {
                                for (index, cell) in cells.iter().enumerate() {
                                    ui.selectable_value(&mut state.selected, index, &cell.label);
                                }
                            })
                            .combo_named(crate::i18n::tr_static("Cell"));
                        if let Some(diff) = state.cells.get(state.selected) {
                            diff.ui(
                                ui,
                                &self.theme,
                                egui::FontId::monospace(self.settings.editor.font_size),
                                "Left cell / outputs",
                                "Right cell / outputs",
                            );
                        }
                    }
                    None => {}
                }
            });
        if ctx.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
            open = false;
        }
        state.visible = open;
        if open || state.rx.is_some() {
            self.diff_workspace.assets = Some(state);
        }
    }
}

#[cfg(test)]
mod configured_external_tests {
    use super::*;
    use crate::diff_view::merge::MergeViewer;
    use std::time::{Duration, Instant};

    fn selected(name: &str) -> PathBuf {
        let path =
            PathBuf::from(std::env::var_os(name).unwrap_or_else(|| panic!("required {name}")));
        assert!(
            path.is_absolute() && path.is_file(),
            "{name} must select a real absolute executable"
        );
        path
    }
    fn start_tick(pid: u32) -> Option<u64> {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        stat.rsplit_once(')')?
            .1
            .split_whitespace()
            .nth(19)?
            .parse()
            .ok()
    }
    fn await_tool(app: &mut IdeApp) -> std::process::ExitStatus {
        let job = app
            .diff_workspace
            .external
            .as_mut()
            .expect("actual external child launched");
        let pid = job.child.id();
        let captured = start_tick(pid)
            .expect("capture the actual owned child, including its zombie before wait");
        let deadline = Instant::now() + Duration::from_secs(10);
        let status = loop {
            if let Some(status) = job.child.try_wait().unwrap() {
                break status;
            }
            if Instant::now() >= deadline {
                let _ = job.child.kill();
                let _ = job.child.wait();
                panic!("admitted real diff/merge tool exceeded deadline");
            }
            std::thread::sleep(Duration::from_millis(5));
        };
        assert_ne!(
            start_tick(pid),
            Some(captured),
            "owned diff/merge child must be physically reaped before publication"
        );
        status
    }
    fn comparison(app: &mut IdeApp, path: &Path, base: &str, ours: &str, theirs: &str) {
        app.diff_workspace.merge = Some((
            path.into(),
            base.into(),
            ours.into(),
            theirs.into(),
            MergeViewer::new(base, theirs, ours, app.settings.diff_merge.clone()),
        ));
    }
    fn viewer(app: &IdeApp) -> &str {
        &app.diff_workspace.merge.as_ref().unwrap().4.result
    }
    fn render_comparison(
        app: &mut IdeApp,
        ctx: &egui::Context,
        path: &Path,
        base: &str,
        ours: &str,
        theirs: &str,
    ) {
        let mut output = ctx.run_ui(Default::default(), |ui| {
            let _ = app.visual_merge_workspace(ui, path, base, ours, theirs);
        });
        // No renderer owns these headless fixture textures, including on a
        // later assertion failure; retire the atlas output before assertions.
        output.textures_delta.clear();
    }

    #[test]
    #[ignore = "requires RUSTPY_EXTERNAL_DIFF_ADMITTED=1, absolute RUSTPY_TEST_DIFF/RUSTPY_TEST_GIT, private RUSTPY_EXTERNAL_DIFF_TEST_ROOT and XDG_CACHE_HOME; Linux"]
    fn configured_real_diff_and_git_merge_publish_only_successful_owned_results() {
        assert!(
            Path::new("/proc/self/stat").is_file(),
            "this acceptance requires real Linux /proc identity, never silently skips"
        );
        assert_eq!(
            std::env::var("RUSTPY_EXTERNAL_DIFF_ADMITTED").as_deref(),
            Ok("1")
        );
        let diff = selected("RUSTPY_TEST_DIFF");
        let git = selected("RUSTPY_TEST_GIT");
        let admitted = PathBuf::from(
            std::env::var_os("RUSTPY_EXTERNAL_DIFF_TEST_ROOT")
                .expect("existing isolated test root required"),
        );
        assert!(admitted.is_absolute() && admitted.is_dir());
        let cache = PathBuf::from(
            std::env::var_os("XDG_CACHE_HOME").expect("explicit isolated cache required"),
        );
        assert!(
            cache.is_absolute() && cache.starts_with(&admitted),
            "merge staging must stay in admitted private cache"
        );
        std::fs::create_dir_all(&cache).unwrap();
        let root = tempfile::tempdir_in(&admitted).unwrap();
        let path = root.path().join("π result with spaces.txt");
        let other = root.path().join("other.txt");
        let base = "head\nshared a\nshared b\nshared c\ntail\n";
        let ours = "HEAD\nshared a\nshared b\nshared c\ntail\n";
        let theirs = "head\nshared a\nshared b\nshared c\nTAIL\n";
        let expected = "HEAD\nshared a\nshared b\nshared c\nTAIL\n";
        std::fs::write(&path, ours).unwrap();
        std::fs::write(&other, theirs).unwrap();
        let mut settings = Settings::default();
        settings.trust_project(root.path());
        settings.diff_merge.external_files = true;
        settings.diff_merge.diff_program = diff.to_string_lossy().into_owned();
        settings.diff_merge.diff_arguments = "-q -- $LEFT$ $RIGHT$".into();
        settings.diff_merge.merge_program = git.to_string_lossy().into_owned();
        // git merge-file edits its first file. RustPy's real staged OUTPUT
        // starts with ours, so no shell redirection or invented output is used.
        settings.diff_merge.merge_arguments = "merge-file --diff3 -- $OUTPUT$ $BASE$ $LEFT$".into();
        settings.diff_merge.auto_nonconflicting = false;
        let mut app = IdeApp::new(settings, Some(root.path().into()));
        app.open_file(&path);
        let tab = app.active.unwrap();
        let ctx = egui::Context::default();
        for (right, expected_code) in [(&path, 0), (&other, 1)] {
            app.status = None;
            app.external_diff_paths(&path, right, false).unwrap();
            assert_eq!(await_tool(&mut app).code(), Some(expected_code));
            app.poll_diff_workspace_jobs(&ctx);
            assert!(app.diff_workspace.external.is_none());
            assert!(
                app.status.is_none(),
                "diff exit 1 means differences, not tool failure"
            );
        }
        app.external_diff_paths(&path, &root.path().join("absent.txt"), false)
            .unwrap();
        assert_eq!(await_tool(&mut app).code(), Some(2));
        app.poll_diff_workspace_jobs(&ctx);
        assert!(app
            .status
            .as_ref()
            .unwrap()
            .0
            .contains("External tool exited"));
        comparison(&mut app, &path, base, ours, theirs);
        let before = viewer(&app).to_owned();
        assert_ne!(
            before, expected,
            "fixture must require actual tool result publication"
        );
        app.tabs[tab].state.doc.set_read_only(true);
        let version = app.tabs[tab].state.doc.version();
        app.start_external_merge(&path, base, ours, theirs).unwrap();
        assert!(
            app.start_external_merge(&path, base, ours, theirs).is_err(),
            "retain exclusive live tool owner"
        );
        assert_eq!(await_tool(&mut app).code(), Some(0));
        assert_eq!(
            viewer(&app),
            before,
            "physical exit alone must not bypass publication poll"
        );
        app.poll_diff_workspace_jobs(&ctx);
        assert_eq!(viewer(&app), expected);
        assert!(app.diff_workspace.external.is_none());
        assert_eq!(app.tabs[tab].state.doc.version(), version);
        assert_eq!(app.tabs[tab].state.doc.text(), ours);
        assert!(app.tabs[tab].state.doc.is_read_only());
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            ours,
            "external result is a reviewable merge buffer, never an automatic source write"
        );

        // Git really writes conflict markers and exits 1. The production poll
        // must not accept this partial/conflicted output as a successful merge.
        let conflict_base = "shared\n";
        let conflict_ours = "ours\n";
        let conflict_theirs = "theirs\n";
        comparison(
            &mut app,
            &path,
            conflict_base,
            conflict_ours,
            conflict_theirs,
        );
        let original = viewer(&app).to_owned();
        app.status = None;
        app.start_external_merge(&path, conflict_base, conflict_ours, conflict_theirs)
            .unwrap();
        assert_eq!(await_tool(&mut app).code(), Some(1));
        let produced = &app
            .diff_workspace
            .external
            .as_ref()
            .unwrap()
            .output
            .as_ref()
            .unwrap()
            .0;
        let conflict = std::fs::read_to_string(produced).unwrap();
        assert!(
            conflict.contains("<<<<<<<")
                && conflict.contains("|||||||")
                && conflict.contains("=======")
                && conflict.contains(">>>>>>>")
        );
        app.poll_diff_workspace_jobs(&ctx);
        assert_eq!(viewer(&app), original);
        assert!(app.diff_workspace.external_result.is_none());
        assert!(app
            .status
            .as_ref()
            .unwrap()
            .0
            .contains("External tool exited"));

        // A new comparison with the same path but changed captured source must
        // never receive an old result; retain it for its exact original tuple.
        comparison(&mut app, &path, base, ours, theirs);
        app.start_external_merge(&path, base, ours, theirs).unwrap();
        let new_ours = "new source snapshot\n";
        app.tabs[tab].state.doc.set_read_only(false);
        app.tabs[tab]
            .state
            .doc
            .set_text(new_ours, &[], &[])
            .unwrap();
        app.tabs[tab].state.doc.set_read_only(true);
        let changed_version = app.tabs[tab].state.doc.version();
        assert!(changed_version > version);
        comparison(&mut app, &path, base, new_ours, theirs);
        let current = viewer(&app).to_owned();
        assert_eq!(await_tool(&mut app).code(), Some(0));
        app.poll_diff_workspace_jobs(&ctx);
        assert_eq!(viewer(&app), current);
        let ((owner, b, o, t), result) = app.diff_workspace.external_result.as_ref().unwrap();
        assert_eq!(
            (owner.as_path(), b.as_str(), o.as_str(), t.as_str()),
            (path.as_path(), base, ours, theirs)
        );
        assert_eq!(result, expected);
        render_comparison(&mut app, &ctx, &path, base, new_ours, theirs);
        assert!(app.diff_workspace.external_result.is_some());
        assert_eq!(viewer(&app), current);
        render_comparison(&mut app, &ctx, &path, base, ours, theirs);
        assert!(app.diff_workspace.external_result.is_none());
        assert_eq!(
            viewer(&app),
            expected,
            "only original comparison consumes retained actual merge result"
        );
        assert_eq!(app.tabs[tab].state.doc.version(), changed_version);
        assert_eq!(app.tabs[tab].state.doc.text(), new_ours);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), ours);
        assert!(!app.diff_workspace_jobs_pending());
    }
}

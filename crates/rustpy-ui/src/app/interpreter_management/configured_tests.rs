//! Explicit real-SDK App publication acceptance, isolated from ordinary tests.
use super::*;
use std::time::{Duration, Instant};

struct Fixture {
    app: IdeApp,
    context: egui::Context,
    time: f64,
    // These directories outlive both the App and every admitted environment job.
    _roots: tempfile::TempDir,
}
impl Fixture {
    fn frame(&mut self, events: Vec<egui::Event>) -> egui::accesskit::TreeUpdate {
        self.time += 0.05;
        let output = self.context.run_ui(
            egui::RawInput {
                events,
                time: Some(self.time),
                focused: true,
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(2000.0, 2400.0),
                )),
                ..Default::default()
            },
            |ui| self.app.interpreter_management_frame(ui.ctx()),
        );
        let tree = output.platform_output.accesskit_update.clone();
        // Retire the atlas before any diagnostic/assertion can unwind.
        output.drop_without_applying_deltas();
        tree.expect("actual manager window must publish an accessibility tree")
    }
    fn button(&mut self, label: &str) -> egui::accesskit::Node {
        let tree = self.frame(Vec::new());
        let nodes: Vec<_> = tree
            .nodes
            .into_iter()
            .filter(|(_, node)| {
                node.role() == egui::accesskit::Role::Button && node.label() == Some(label)
            })
            .collect();
        assert_eq!(nodes.len(), 1, "one actual {label} button");
        nodes.into_iter().next().unwrap().1
    }
    fn click(&mut self, label: &str) {
        let bounds = self.button(label).bounds().expect("actual button bounds");
        let point = egui::pos2(
            ((bounds.x0 + bounds.x1) / 2.0) as f32,
            ((bounds.y0 + bounds.y1) / 2.0) as f32,
        );
        for pressed in [true, false] {
            self.frame(vec![
                egui::Event::PointerMoved(point),
                egui::Event::PointerButton {
                    pos: point,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                },
            ]);
        }
    }
    fn finish(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(30);
        while self.app.interpreter_management.task.is_some() {
            self.frame(Vec::new());
            assert!(
                Instant::now() < deadline,
                "actual environment probe/publication did not finish: {:?}",
                self.app.interpreter_management.message
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        if let Some((_, task)) = self.app.interpreter_management.task.take() {
            task.cancel();
            // Task::Finished follows the real bounded interpreter probe's return.
            // Do not remove either project while its owned probe can still run.
            loop {
                if matches!(task.poll(), Some(Event::Finished(_))) {
                    break;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        self.app.cancel_interpreter_management();
    }
}

#[test]
#[ignore = "requires RUSTPY_APP_INTERPRETER_ADMITTED=yes, explicit private real venv Python and fresh private RUSTPY_CONFIG_DIR; run alone"]
fn configured_add_interpreter_widget_publishes_real_sdk_and_preserves_project_authority() {
    assert_eq!(
        std::env::var("RUSTPY_APP_INTERPRETER_ADMITTED").as_deref(),
        Ok("yes")
    );
    assert!(
        !rustpy_settings::trust_all_from_env(),
        "safe-mode control requires trust bypass disabled"
    );
    let python = PathBuf::from(
        std::env::var_os("RUSTPY_APP_INTERPRETER_PYTHON")
            .expect("explicit genuine disposable venv Python required"),
    );
    let sdk_root = PathBuf::from(
        std::env::var_os("RUSTPY_APP_INTERPRETER_SDK_ROOT")
            .expect("explicit private venv root required"),
    );
    assert!(python.is_absolute() && python.is_file() && sdk_root.is_absolute());
    assert!(
        python.starts_with(&sdk_root) && sdk_root.join("pyvenv.cfg").is_file(),
        "admit a genuine existing private venv, never a generated interpreter shim"
    );
    // Admitted roots come from the environment and default to TMPDIR itself:
    // no fixed /tmp paths or magic subdirectory names are required.
    let admitted_root = |variable: &str| {
        let root = std::env::var_os(variable)
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        root.canonicalize()
            .unwrap_or_else(|e| panic!("admitted {variable} root {root:?}: {e}"))
    };
    let phase = admitted_root("RUSTPY_APP_INTERPRETER_PHASE_ROOT");
    let cleanup = admitted_root("RUSTPY_APP_INTERPRETER_CLEANUP_ROOT");
    assert!(
        sdk_root.canonicalize().unwrap().starts_with(&phase),
        "SDK must be explicitly owned beneath the admitted phase root"
    );
    let config = PathBuf::from(
        std::env::var_os("RUSTPY_CONFIG_DIR").expect("fresh private config directory required"),
    );
    assert!(config.is_absolute() && config.is_dir());
    let config = config.canonicalize().unwrap();
    assert!(config.starts_with(&cleanup) || config.starts_with(&phase));
    assert!(
        !registry_path().exists(),
        "run exactly this ignored case in its own fresh profile"
    );
    let roots = tempfile::Builder::new()
        .prefix("configured-app-sdk-λ-")
        .tempdir()
        .unwrap();
    let first = roots.path().join("first");
    let second = roots.path().join("second");
    std::fs::create_dir(&first).unwrap();
    std::fs::create_dir(&second).unwrap();
    // Start untrusted so automatic discovery never probes unadmitted candidates.
    // Only the selected actual manager request below is admitted for execution.
    let app = IdeApp::new(Settings::default(), Some(first.clone()));
    let context = egui::Context::default();
    crate::theme::install_fonts(&context);
    context.enable_accesskit();
    context.all_styles_mut(|style| style.animation_time = 0.0);
    let mut f = Fixture {
        app,
        context,
        time: 0.0,
        _roots: roots,
    };
    assert!(f.app.project_safe_mode());
    f.app.open_interpreter_management();
    {
        let state = &mut f.app.interpreter_management;
        state.manager = Manager::System;
        state.existing = true;
        state.executable = python.display().to_string();
        state.location = python.display().to_string();
        state.name = "Admitted private SDK λ".into();
        state.shared = false;
        state.install = false;
    }
    assert!(f.button("Add interpreter").is_disabled());
    f.click("Add interpreter");
    assert!(f.app.interpreter_management.task.is_none());
    assert!(!registry_path().exists());
    // Captured trusted-project input for this feature, without invoking unrelated
    // global rediscovery/terminal startup. This is not Trust-dialog acceptance.
    f.app.settings.trust_project(&first);
    f.app.safe_mode = false;
    assert!(!f.app.project_safe_mode());
    assert!(!f.button("Add interpreter").is_disabled());
    f.click("Add interpreter");
    assert!(
        f.app.interpreter_management.task.is_some(),
        "the real Add button must submit the actual manager request"
    );
    f.finish();
    let selected = f
        .app
        .project
        .as_ref()
        .unwrap()
        .interpreter
        .as_ref()
        .expect("real probe must select the SDK");
    assert_eq!(selected.path, python);
    assert_eq!(
        selected.prefix.canonicalize().unwrap(),
        sdk_root.canonicalize().unwrap()
    );
    assert!(selected.is_venv && !selected.sys_path.is_empty() && !selected.version.is_empty());
    assert_eq!(
        rustpy_project::ProjectModel::load(&first).sdk.as_ref(),
        Some(&python)
    );
    assert_eq!(
        rustpy_settings::Workspace::load(&first)
            .interpreter
            .as_ref(),
        Some(&python)
    );
    assert_eq!(
        f.app.effective_workspace().interpreter.as_ref(),
        Some(&python)
    );
    let registry = Registry::load(&registry_path());
    let entry = registry
        .entries
        .iter()
        .find(|entry| entry.interpreter.path == python)
        .unwrap();
    assert_eq!(entry.name, "Admitted private SDK λ");
    assert!(!entry.shared && entry.projects.contains(&first) && !entry.projects.contains(&second));
    assert_eq!(
        f.app.interpreter_management.selected.as_ref(),
        Some(&python)
    );

    // A second genuine widget request has captured the first project. Replace
    // the active owner before the real result is polled, without inventing an
    // Event or Interpreter. The new Project is constructed by the public App
    // path; hold the original until completion so its files cannot disappear.
    f.click("Use for project");
    assert!(f.app.interpreter_management.task.is_some());
    let mut replacement = IdeApp::new(Settings::default(), Some(second.clone()));
    let old_project = f.app.project.replace(replacement.project.take().unwrap());
    f.app.project_trust_identity = replacement.project_trust_identity.take();
    f.app.safe_mode = true;
    f.finish();
    assert_eq!(f.app.project.as_ref().unwrap().root, second);
    assert!(
        f.app.project.as_ref().unwrap().interpreter.is_none(),
        "late result must not select an SDK in a different project"
    );
    assert!(rustpy_project::ProjectModel::load(&second).sdk.is_none());
    assert!(rustpy_settings::Workspace::load(&second)
        .interpreter
        .is_none());
    let registry = Registry::load(&registry_path());
    let entry = registry
        .entries
        .iter()
        .find(|entry| entry.interpreter.path == python)
        .unwrap();
    assert!(entry.projects.contains(&first) && !entry.projects.contains(&second));
    drop(old_project);
}

#[test]
#[ignore = "requires RUSTPY_APP_INTERPRETER_MANAGER_ADMITTED=yes, an installed Conda/Pipenv/Hatch/PDM CLI, base Python, private provider caches and fresh private RUSTPY_CONFIG_DIR; run alone"]
fn configured_add_interpreter_widget_creates_real_manager_environment() {
    assert_eq!(
        std::env::var("RUSTPY_APP_INTERPRETER_MANAGER_ADMITTED").as_deref(),
        Ok("yes")
    );
    let selected = |key: &str| {
        let path = PathBuf::from(std::env::var_os(key).unwrap_or_else(|| panic!("{key}")));
        assert!(path.is_absolute() && path.is_file(), "{key}: {path:?}");
        path
    };
    let manager = match std::env::var("RUSTPY_APP_INTERPRETER_MANAGER").as_deref() {
        Ok("conda") => Manager::Conda,
        Ok("pipenv") => Manager::Pipenv,
        Ok("hatch") => Manager::Hatch,
        Ok("pdm") => Manager::Pdm,
        other => panic!("select conda/pipenv/hatch/pdm, got {other:?}"),
    };
    let executable = selected("RUSTPY_APP_INTERPRETER_MANAGER_EXECUTABLE");
    let base = selected("RUSTPY_APP_INTERPRETER_BASE");
    let temporary = std::env::temp_dir().canonicalize().unwrap();
    let config = PathBuf::from(
        std::env::var_os("RUSTPY_CONFIG_DIR").expect("fresh private config directory required"),
    );
    assert!(config.is_dir() && config.canonicalize().unwrap().starts_with(&temporary));
    assert!(
        !registry_path().exists(),
        "run exactly this ignored case in its own fresh profile"
    );
    let roots = tempfile::Builder::new()
        .prefix("configured-app-manager-λ-")
        .tempdir()
        .unwrap();
    let project = roots.path().canonicalize().unwrap().join("managed project");
    std::fs::create_dir(&project).unwrap();
    std::fs::write(project.join("Pipfile"), "[packages]\n[dev-packages]\n").unwrap();
    std::fs::write(
        project.join("pyproject.toml"),
        "[project]\nname='app-managed'\nversion='1.0'\nrequires-python='>=3.8'\ndependencies=[]\n[tool.hatch.envs.default]\nskip-install=true\n[tool.pdm]\ndistribution=false\n",
    )
    .unwrap();
    let app = IdeApp::new(Settings::default(), Some(project.clone()));
    let context = egui::Context::default();
    crate::theme::install_fonts(&context);
    context.enable_accesskit();
    context.all_styles_mut(|style| style.animation_time = 0.0);
    let mut f = Fixture {
        app,
        context,
        time: 0.0,
        _roots: roots,
    };
    f.app.settings.trust_project(&project);
    f.app.safe_mode = false;
    f.app.open_interpreter_management();
    // Conda validates the prefix leaf as an environment name (no spaces).
    let location = project.join(if manager == Manager::Conda {
        "condaenv"
    } else {
        ".venv"
    });
    {
        let state = &mut f.app.interpreter_management;
        state.manager = manager;
        state.existing = false;
        state.executable = executable.display().to_string();
        state.base = base.display().to_string();
        state.location = location.display().to_string();
        state.version = std::env::var("RUSTPY_CONDA_PYTHON_VERSION").unwrap_or_default();
        state.name = format!("{} SDK λ", manager.name());
        state.shared = false;
        state.install = false;
    }
    // Let the window finish its sizing pass so the click lands on the button.
    for _ in 0..3 {
        f.frame(Vec::new());
    }
    assert!(!f.app.project_safe_mode(), "trusted project");
    assert!(!f.button("Create environment").is_disabled());
    f.click("Create environment");
    assert!(
        f.app.interpreter_management.task.is_some(),
        "the real Create environment button must submit the manager request: {:?}",
        f.app.interpreter_management.message
    );
    let deadline = Instant::now() + Duration::from_secs(600);
    while f.app.interpreter_management.task.is_some() {
        f.frame(Vec::new());
        assert!(Instant::now() < deadline, "manager did not finish");
        std::thread::sleep(Duration::from_millis(20));
    }
    let interpreter = f
        .app
        .project
        .as_ref()
        .unwrap()
        .interpreter
        .clone()
        .unwrap_or_else(|| {
            panic!(
                "real {} environment must be selected: {:?}",
                manager.name(),
                f.app.interpreter_management.message
            )
        });
    assert!(!interpreter.version.is_empty() && !interpreter.sys_path.is_empty());
    assert!(interpreter.path.is_file());
    assert_ne!(interpreter.path, base, "a new environment, not the base");
    let prefix = interpreter.prefix.canonicalize().unwrap();
    assert!(prefix.starts_with(&temporary), "{prefix:?}");
    if matches!(manager, Manager::Conda | Manager::Pipenv | Manager::Pdm) {
        assert_eq!(prefix, location.canonicalize().unwrap());
    }
    assert_eq!(
        rustpy_project::ProjectModel::load(&project).sdk.as_ref(),
        Some(&interpreter.path)
    );
    let registry = Registry::load(&registry_path());
    let entry = registry
        .entries
        .iter()
        .find(|entry| entry.interpreter.path == interpreter.path)
        .expect("registry entry");
    assert_eq!(entry.name, format!("{} SDK λ", manager.name()));
    assert!(entry.projects.contains(&project));
    eprintln!(
        "{} created {} ({})",
        manager.name(),
        interpreter.path.display(),
        interpreter.version
    );
}

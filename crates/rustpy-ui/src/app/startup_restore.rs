//! Checked restoration preparation before logical window acceptance.
use super::*;
use rustpy_core::vfs::{ContentRevision, ReadLimits, Snapshot, Vfs};
use std::fs::{File, OpenOptions};

struct StartupDirectory {
    path: PathBuf,
    #[cfg(any(unix, windows))]
    handle: File,
}
impl StartupDirectory {
    fn open(path: &Path) -> Result<Self, String> {
        #[cfg(unix)]
        let handle = {
            use std::os::unix::fs::OpenOptionsExt;
            OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
                .open(path)
                .map_err(|error| error.to_string())?
        };
        #[cfg(windows)]
        let handle = {
            use std::os::windows::fs::OpenOptionsExt;
            OpenOptions::new()
                .read(true)
                .share_mode(3)
                .custom_flags(0x0200_0000 | 0x0020_0000)
                .open(path)
                .map_err(|error| error.to_string())?
        };
        let directory = Self {
            path: path.into(),
            #[cfg(any(unix, windows))]
            handle,
        };
        directory.validate()?;
        Ok(directory)
    }
    fn validate(&self) -> Result<(), String> {
        let current = std::fs::symlink_metadata(&self.path).map_err(|error| error.to_string())?;
        if !current.is_dir() || current.file_type().is_symlink() {
            return Err("Prepared project directory was replaced".into());
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let owned = self.handle.metadata().map_err(|error| error.to_string())?;
            if !owned.is_dir() || owned.dev() != current.dev() || owned.ino() != current.ino() {
                return Err("Prepared project directory identity changed".into());
            }
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            // The retained handle denies directory deletion and replacement.
            let owned = self.handle.metadata().map_err(|error| error.to_string())?;
            if !owned.is_dir()
                || owned.file_attributes() & 0x400 != 0
                || current.file_attributes() & 0x400 != 0
            {
                return Err("Prepared project directory was replaced".into());
            }
        }
        Ok(())
    }
}

fn project_snapshot(root: &Path) -> Result<Option<Snapshot>, String> {
    let path = rustpy_project::ProjectModel::path_for(root);
    match std::fs::symlink_metadata(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.to_string()),
        Ok(metadata) if !metadata.is_file() => {
            return Err("Project metadata is not a regular file".into())
        }
        Ok(_) => {}
    }
    Vfs::new()
        .read(
            &path,
            ReadLimits {
                max_file_bytes: 8 * 1024 * 1024,
                max_cached_bytes: 0,
                max_cached_files: 0,
            },
        )
        .map(Some)
        .map_err(|error| error.to_string())
}

pub(crate) struct PreparedStartupProject {
    pub(super) root: PathBuf,
    directory: Arc<StartupDirectory>,
    model_revision: Option<ContentRevision>,
    pub(super) workspace: workspace_ui::WorkspaceClient,
    pub(super) model: rustpy_project::ProjectModel,
    pub(super) ignored_roots: Vec<String>,
}

impl IdeApp {
    pub(crate) fn prepare_startup_project(root: &Path) -> Result<PreparedStartupProject, String> {
        let root = std::fs::canonicalize(root).map_err(|error| error.to_string())?;
        if !root.is_dir() {
            return Err("Restored project is not a directory".into());
        }
        let directory = Arc::new(StartupDirectory::open(&root)?);
        let workspace = workspace_ui::WorkspaceClient::open_checked(&root)?;
        let snapshot = project_snapshot(&root)?;
        let model_revision = snapshot.as_ref().map(|snapshot| snapshot.revision);
        let (model, ignored_roots) = rustpy_project::ProjectModel::from_snapshot_reporting(
            &root,
            snapshot.as_ref().map(|snapshot| snapshot.bytes.as_ref()),
            &workspace.effective(),
        )?;
        if project_snapshot(&root)?
            .as_ref()
            .map(|snapshot| snapshot.revision)
            != model_revision
        {
            return Err("Project metadata changed during startup preparation".into());
        }
        workspace.validate_checked()?;
        directory.validate()?;
        Ok(PreparedStartupProject {
            root,
            directory,
            model_revision,
            workspace,
            model,
            ignored_roots,
        })
    }

    pub(crate) fn install_startup_project(
        &mut self,
        prepared: PreparedStartupProject,
    ) -> Result<(), String> {
        if self.project.is_some() || !self.tabs.is_empty() || self.workspace_client.is_some() {
            return Err("Restoration requires an empty startup window".into());
        }
        prepared.directory.validate()?;
        prepared.workspace.validate_checked()?;
        if project_snapshot(&prepared.root)?
            .as_ref()
            .map(|snapshot| snapshot.revision)
            != prepared.model_revision
        {
            return Err("Project metadata changed after startup preparation".into());
        }
        prepared.workspace.validate_checked()?;
        prepared.directory.validate()?;
        let root = prepared.root.clone();
        let _directory = Arc::clone(&prepared.directory);
        self.open_project_prepared(&root, Some(prepared));
        if self.actual_current_project_root() != Some(root.as_path()) {
            return Err("Prepared project was not installed".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn welcome_constructor_child() {
        let Some(recent) = std::env::var_os("RUSTPY_WELCOME_CONSTRUCTOR_RECENT") else {
            return;
        };
        let settings = rustpy_settings::Settings {
            reopen_last_project: true,
            recent_projects: vec![PathBuf::from(recent)],
            ..Default::default()
        };
        let app = IdeApp::new_welcome(settings);
        assert!(app.actual_current_project_root().is_none());
        assert!(app.tabs.is_empty());
        assert!(app.settings.reopen_last_project);
        assert!(!rustpy_settings::Settings::config_dir()
            .join("settings.json")
            .exists());
        assert!(!std::env::current_dir()
            .unwrap()
            .join(".rustpy/workspace.json")
            .exists());
        assert!(!app.settings.recent_projects[0]
            .join(".rustpy/workspace.json")
            .exists());
    }

    #[test]
    fn welcome_constructor_ignores_recent_project_and_project_like_cwd() {
        let root = tempfile::tempdir().unwrap();
        let cwd = root.path().join("cwd");
        let recent = root.path().join("recent");
        let config = root.path().join("config");
        std::fs::create_dir_all(&cwd).unwrap();
        std::fs::create_dir_all(&recent).unwrap();
        std::fs::write(cwd.join("pyproject.toml"), "[project]\nname = 'cwd'\n").unwrap();
        std::fs::write(
            recent.join("pyproject.toml"),
            "[project]\nname = 'recent'\n",
        )
        .unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "app::startup_restore::tests::welcome_constructor_child",
                "--exact",
                "--nocapture",
            ])
            .current_dir(&cwd)
            .env("RUSTPY_WELCOME_CONSTRUCTOR_RECENT", &recent)
            .env("RUSTPY_CONFIG_DIR", &config)
            .env("RUSTPY_SYSTEM_DIR", root.path().join("system"))
            .env("RUSTPY_LOG_DIR", root.path().join("log"))
            .env("XDG_CONFIG_HOME", root.path().join("xdg-config"))
            .env("XDG_CACHE_HOME", root.path().join("xdg-cache"))
            .env_remove("DISPLAY")
            .env_remove("WAYLAND_DISPLAY")
            .stdin(std::process::Stdio::piped())
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        assert!(!config.join("settings.json").exists());
        assert!(!cwd.join(".rustpy/workspace.json").exists());
        assert!(!recent.join(".rustpy/workspace.json").exists());
    }

    #[test]
    fn invalid_workspace_or_model_never_installs_a_restored_project() {
        for file in ["workspace.json", "project.json"] {
            let root = tempfile::tempdir().unwrap();
            std::fs::create_dir_all(root.path().join(".rustpy")).unwrap();
            let path = root.path().join(".rustpy").join(file);
            let bytes = b"owned invalid JSON\xff";
            std::fs::write(&path, bytes).unwrap();
            let app = IdeApp::new_welcome(Settings::default());
            assert!(
                IdeApp::prepare_startup_project(root.path()).is_err(),
                "{file}"
            );
            assert!(app.project.is_none());
            assert!(app.tabs.is_empty());
            assert!(app.workspace_client.is_none());
            assert_eq!(std::fs::read(&path).unwrap(), bytes);
        }
    }

    #[test]
    fn changed_prepared_workspace_or_model_is_rejected_before_installation() {
        for file in ["workspace.json", "project.json"] {
            let root = tempfile::tempdir().unwrap();
            let prepared = IdeApp::prepare_startup_project(root.path()).unwrap();
            let path = root.path().join(".rustpy").join(file);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            let bytes = b"changed after preparation";
            std::fs::write(&path, bytes).unwrap();
            let mut app = IdeApp::new_welcome(Settings::default());
            assert!(app.install_startup_project(prepared).is_err(), "{file}");
            assert!(app.project.is_none());
            assert!(app.tabs.is_empty());
            assert!(app.workspace_client.is_none());
            assert_eq!(std::fs::read(&path).unwrap(), bytes);
            assert!(rustpy_settings::WorkspaceStore::new(root.path())
                .try_acquire_session()
                .unwrap()
                .is_some());
        }
    }

    #[cfg(unix)]
    #[test]
    fn removed_prepared_directory_is_rejected_before_installation() {
        let container = tempfile::tempdir().unwrap();
        let root = container.path().join("project");
        std::fs::create_dir(&root).unwrap();
        let prepared = IdeApp::prepare_startup_project(&root).unwrap();
        std::fs::rename(&root, container.path().join("moved")).unwrap();
        let mut app = IdeApp::new_welcome(Settings::default());
        assert!(app.install_startup_project(prepared).is_err());
        assert!(app.project.is_none());
        assert!(app.tabs.is_empty());
        assert!(app.workspace_client.is_none());
        assert!(!root.exists());
    }

    #[test]
    fn equivalent_but_new_project_metadata_is_still_stale() {
        let root = tempfile::tempdir().unwrap();
        let prepared = IdeApp::prepare_startup_project(root.path()).unwrap();
        let path = rustpy_project::ProjectModel::path_for(root.path());
        std::fs::write(&path, b"{}").unwrap();
        let mut app = IdeApp::new_welcome(Settings::default());
        assert!(app.install_startup_project(prepared).is_err());
        assert!(app.project.is_none());
        assert!(app.tabs.is_empty());
        assert!(app.workspace_client.is_none());
        assert_eq!(std::fs::read(path).unwrap(), b"{}");
    }

    #[cfg(unix)]
    #[test]
    fn replaced_prepared_directory_does_not_install_the_replacement() {
        let container = tempfile::tempdir().unwrap();
        let root = container.path().join("project");
        std::fs::create_dir(&root).unwrap();
        let prepared = IdeApp::prepare_startup_project(&root).unwrap();
        std::fs::rename(&root, container.path().join("original")).unwrap();
        std::fs::create_dir(&root).unwrap();
        let mut app = IdeApp::new_welcome(Settings::default());
        assert!(app.install_startup_project(prepared).is_err());
        assert!(app.project.is_none());
        assert!(app.tabs.is_empty());
        assert!(app.workspace_client.is_none());
        assert!(!root.join(".rustpy").exists());
    }

    #[cfg(unix)]
    #[test]
    fn replaced_workspace_lease_is_rejected_without_installing_or_writing() {
        let root = tempfile::tempdir().unwrap();
        let prepared = IdeApp::prepare_startup_project(root.path()).unwrap();
        let lock = root.path().join(".rustpy/workspace.session.lock");
        std::fs::remove_file(&lock).unwrap();
        let competitor = rustpy_settings::WorkspaceStore::new(root.path());
        let competing_lease = competitor.try_acquire_session().unwrap().unwrap();
        let mut app = IdeApp::new_welcome(Settings::default());
        assert!(app.install_startup_project(prepared).is_err());
        assert!(app.project.is_none());
        assert!(app.tabs.is_empty());
        assert!(app.workspace_client.is_none());
        assert!(!rustpy_settings::Workspace::path_for(root.path()).exists());
        competitor.validate_session(&competing_lease).unwrap();
        assert!(competitor.try_acquire_session().unwrap().is_none());
    }

    #[test]
    fn installation_retains_the_prepared_workspace_session() {
        let root = tempfile::tempdir().unwrap();
        let prepared = IdeApp::prepare_startup_project(root.path()).unwrap();
        let competitor = rustpy_settings::WorkspaceStore::new(root.path());
        assert!(competitor.try_acquire_session().unwrap().is_none());
        let mut app = IdeApp::new_welcome(Settings::default());
        app.install_startup_project(prepared).unwrap();
        assert_eq!(app.actual_current_project_root(), Some(root.path()));
        assert!(competitor.try_acquire_session().unwrap().is_none());
        assert!(app.workspace_client.is_some());
    }

    #[test]
    fn checked_preparation_holds_workspace_ownership_without_project_installation() {
        let root = tempfile::tempdir().unwrap();
        let prepared = IdeApp::prepare_startup_project(root.path()).unwrap();
        let competitor = rustpy_settings::WorkspaceStore::new(root.path());
        assert!(competitor.try_acquire_session().unwrap().is_none());
        drop(prepared);
        assert!(competitor.try_acquire_session().unwrap().is_some());
        assert!(!rustpy_settings::Workspace::path_for(root.path()).exists());
    }
}

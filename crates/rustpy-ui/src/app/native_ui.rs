//! Opt-in native panels with exact request/workspace/document ownership.
//! Dialog subprocesses run only on TaskManager workers and remain retained
//! through cancellation, project transitions and managed-quit retirement.
use super::*;
use crate::a11y::DialogWindow;
use std::time::Duration;
mod platform;
use platform::Kind;
#[derive(Clone, Debug, PartialEq, Eq)]
struct Owner {
    session: u64,
    root: Option<PathBuf>,
}
#[derive(Clone)]
struct DocumentOwner {
    id: rustpy_text::DocumentId,
    version: u64,
    path: Option<PathBuf>,
    notebook: Option<String>,
}
#[derive(Clone)]
enum Target {
    File,
    Project,
    Copy(DocumentOwner),
}
impl Target {
    fn kind(&self) -> Kind {
        match self {
            Self::File => Kind::File,
            Self::Project => Kind::Directory,
            Self::Copy(_) => Kind::Save,
        }
    }
}
struct Pending {
    id: u64,
    task: rustpy_core::TaskHandle<Option<PathBuf>>,
    owner: Owner,
    target: Target,
    directory: PathBuf,
    name: String,
}
struct Browser {
    owner: Owner,
    target: Target,
    browser: FileBrowser,
    name: String,
    error: Option<String>,
}
struct Overwrite {
    owner: Owner,
    document: DocumentOwner,
    path: PathBuf,
    bytes: Vec<u8>,
    expected: Option<rustpy_core::vfs::ContentRevision>,
}
#[derive(Default)]
pub(super) struct State {
    serial: u64,
    pending: Vec<Pending>,
    browser: Option<Browser>,
    overwrite: Option<Overwrite>,
    builtin_open: bool,
}
impl Drop for State {
    fn drop(&mut self) {
        for pending in &self.pending {
            pending.task.cancel();
        }
    }
}
impl IdeApp {
    fn native_owner(&self) -> Owner {
        Owner {
            session: self.file_session,
            root: self.project.as_ref().map(|project| project.root.clone()),
        }
    }
    fn native_document_current(&self, capture: &DocumentOwner) -> Option<usize> {
        self.tabs.iter().position(|tab| {
            tab.state.doc.identity() == capture.id
                && tab.state.doc.version() == capture.version
                && tab.state.path == capture.path
                && tab
                    .notebook
                    .as_ref()
                    .filter(|notebook| notebook.is_dirty())
                    .map(|notebook| notebook.text())
                    == capture.notebook
        })
    }
    pub(super) fn native_ui_settings(&mut self, ui: &mut egui::Ui) {
        ui.checkbox(
            &mut self.settings.native_ui.file_chooser,
            "Use native operating-system file dialogs",
        );
        ui.small("Open files, choose project folders and save copies with the system dialog. If a provider is unavailable, use the built-in file browser. Cancelling a dialog never opens a fallback.");
        #[cfg(target_os = "linux")]
        egui::ComboBox::from_label("Native Linux dialog provider")
            .selected_text(format!("{:?}", self.settings.native_ui.linux_provider))
            .show_ui(ui, |ui| {
                use rustpy_settings::native_ui::LinuxProvider;
                for (provider, label) in [
                    (LinuxProvider::Auto, "Desktop preference (Zenity / KDialog)"),
                    (LinuxProvider::Zenity, "Zenity"),
                    (LinuxProvider::Kdialog, "KDialog"),
                ] {
                    ui.selectable_value(
                        &mut self.settings.native_ui.linux_provider,
                        provider,
                        crate::i18n::tr_static(label),
                    );
                }
            });
        #[cfg(target_os = "linux")]
        for (label, selected) in [
            (
                "Zenity executable (empty uses /usr/bin/zenity)",
                &mut self.settings.native_ui.zenity_program,
            ),
            (
                "KDialog executable (empty uses /usr/bin/kdialog)",
                &mut self.settings.native_ui.kdialog_program,
            ),
        ] {
            ui.label(crate::i18n::tr_static(label));
            let mut text = selected
                .as_ref()
                .map(|path| path.to_string_lossy().into_owned())
                .unwrap_or_default();
            if ui.text_edit_singleline(&mut text).changed() {
                *selected = (!text.is_empty()).then(|| PathBuf::from(text));
            }
        }
    }
    /// Optional File menu extension; the canonical Open action also follows
    /// the native toggle through native_open_path_dialog.
    pub(super) fn native_ui_file_menu(&mut self, ui: &mut egui::Ui) {
        if ui.button("Open File…").clicked() {
            let directory = self.native_initial_directory();
            self.begin_native_choice(Target::File, directory, String::new());
            ui.close();
        }
        if ui.button("Open Project Folder…").clicked() {
            let directory = self.native_initial_directory();
            self.begin_native_choice(Target::Project, directory, String::new());
            ui.close();
        }
        if ui
            .add_enabled(
                self.active
                    .and_then(|index| self.tabs.get(index))
                    .is_some_and(|tab| tab.image.is_none()),
                egui::Button::new("Save a Copy…"),
            )
            .clicked()
        {
            self.save_copy_chooser();
            ui.close();
        }
    }
    fn native_initial_directory(&self) -> PathBuf {
        self.active
            .and_then(|index| self.tabs.get(index))
            .and_then(|tab| tab.state.path.as_ref())
            .and_then(|path| path.parent())
            .map(Path::to_path_buf)
            .or_else(|| self.project.as_ref().map(|project| project.root.clone()))
            .or_else(dirs::home_dir)
            .unwrap_or_else(std::env::temp_dir)
    }
    pub(super) fn open_path_chooser(&mut self, directory: PathBuf) {
        self.cancel_native_ui();
        self.native_ui.builtin_open = false;
        self.dialog = Dialog::OpenPath(Box::new(FileBrowser::new(directory)));
    }
    /// True when the opt-in native mode owns this Open dialog. The built-in
    /// browser remains available without changing the user's preference.
    pub(super) fn native_open_path_dialog(
        &mut self,
        ctx: &egui::Context,
        browser: &FileBrowser,
    ) -> bool {
        if !self.settings.native_ui.file_chooser || self.native_ui.builtin_open {
            return false;
        }
        let mut open = true;
        let mut choice = None;
        let mut builtin = false;
        egui::Window::new("Open File or Project")
            .collapsible(false)
            .open(&mut open)
            .show_modal(ctx, |ui| {
                ui.label("Choose what to open with the native dialog.");
                ui.horizontal(|ui| {
                    if ui.button("File…").clicked() {
                        choice = Some(Target::File);
                    }
                    if ui.button("Project Folder…").clicked() {
                        choice = Some(Target::Project);
                    }
                    if ui.button("Built-in Browser").clicked() {
                        builtin = true;
                    }
                });
            });
        if builtin {
            self.native_ui.builtin_open = true;
            self.dialog = Dialog::OpenPath(Box::new(browser.clone()));
        } else if let Some(target) = choice {
            self.begin_native_choice(target, browser.dir.clone(), String::new());
        } else if open {
            self.dialog = Dialog::OpenPath(Box::new(browser.clone()));
        } else {
            self.focus_editor = true;
        }
        true
    }
    pub(super) fn save_copy_chooser(&mut self) {
        let Some(index) = self.active else {
            return;
        };
        let tab = &self.tabs[index];
        if tab.image.is_some() {
            self.set_status("Save a Copy supports text editors and notebooks");
            return;
        }
        let capture = DocumentOwner {
            id: tab.state.doc.identity(),
            version: tab.state.doc.version(),
            path: tab.state.path.clone(),
            notebook: tab
                .notebook
                .as_ref()
                .filter(|notebook| notebook.is_dirty())
                .map(|notebook| notebook.text()),
        };
        let name = tab
            .state
            .path
            .as_ref()
            .and_then(|path| path.file_name())
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "untitled.txt".into());
        let directory = self.native_initial_directory();
        self.begin_native_choice(Target::Copy(capture), directory, name);
    }
    fn begin_native_choice(&mut self, target: Target, directory: PathBuf, name: String) {
        self.cancel_native_ui();
        if self.native_ui.pending.len() >= 8 {
            self.set_status("Waiting for cancelled native dialogs to close");
            return;
        }
        let owner = self.native_owner();
        if !self.settings.native_ui.file_chooser {
            self.native_ui.browser = Some(Browser {
                owner,
                target,
                browser: FileBrowser::new(directory),
                name,
                error: None,
            });
            return;
        }
        let settings = self.settings.native_ui.clone();
        let (worker_directory, worker_name) = (directory.clone(), name.clone());
        let kind = target.kind();
        let task = rustpy_core::TaskManager::global()
            .task("Native file dialog")
            .spawn(move |progress| {
                platform::choose(
                    &settings,
                    kind,
                    &worker_directory,
                    &worker_name,
                    progress.token(),
                )
                .map_err(rustpy_core::TaskError::Failed)
            });
        let id = self.native_ui.serial;
        self.native_ui.pending.push(Pending {
            id,
            task,
            owner,
            target,
            directory,
            name,
        });
        self.dialog = Dialog::None;
    }
    pub(super) fn native_ui_jobs(&self) -> Vec<(u64, String)> {
        self.native_ui
            .pending
            .iter()
            .map(|pending| (pending.task.id(), "Native file dialog".into()))
            .collect()
    }
    pub(super) fn stop_native_ui_job(&mut self, id: u64) -> bool {
        if let Some(pending) = self
            .native_ui
            .pending
            .iter()
            .find(|pending| pending.task.id() == id)
        {
            pending.task.cancel();
            true
        } else {
            false
        }
    }
    pub(super) fn cancel_native_ui(&mut self) {
        self.native_ui.serial = self.native_ui.serial.wrapping_add(1);
        for pending in &self.native_ui.pending {
            pending.task.cancel();
        }
        self.native_ui.browser = None;
        self.native_ui.overwrite = None;
    }
    /// Poll-only while a quit guard is prepared. Never draws, publishes a
    /// path, writes a file, dispatches another chooser or reopens a fallback.
    pub(super) fn retire_native_ui(&mut self, ctx: &egui::Context) -> bool {
        self.cancel_native_ui();
        self.native_ui
            .pending
            .retain_mut(|pending| pending.task.try_result().is_none());
        let busy = !self.native_ui.pending.is_empty();
        if busy {
            ctx.request_repaint_after(Duration::from_millis(25));
        }
        busy
    }
    pub(super) fn native_ui_tick(&mut self, ctx: &egui::Context) {
        let owner = self.native_owner();
        let mut completed = Vec::new();
        for (index, pending) in self.native_ui.pending.iter_mut().enumerate() {
            if pending.owner != owner {
                pending.task.cancel();
            }
            if let Some(result) = pending.task.try_result() {
                completed.push((index, result));
            }
        }
        for (index, result) in completed.into_iter().rev() {
            let pending = self.native_ui.pending.remove(index);
            if pending.id != self.native_ui.serial
                || pending.task.is_cancelled()
                || pending.owner != owner
            {
                continue;
            }
            if !matches!(self.dialog, Dialog::None) {
                self.set_status("Native chooser result discarded because another dialog is open");
                continue;
            }
            match result {
                Ok(Some(path)) => self.apply_native_choice(pending.owner, pending.target, path),
                Ok(None) => self.focus_editor = true,
                Err(error) => {
                    self.native_ui.browser = Some(Browser {
                        owner: pending.owner,
                        target: pending.target,
                        browser: FileBrowser::new(pending.directory),
                        name: pending.name,
                        error: Some(format!("{error}. Choose a path with the built-in browser.")),
                    });
                }
            }
        }
        if !self.native_ui.pending.is_empty() {
            ctx.request_repaint_after(Duration::from_millis(50));
        }
        if self
            .native_ui
            .browser
            .as_ref()
            .is_some_and(|browser| browser.owner != owner)
        {
            self.native_ui.browser = None;
        }
        if self.native_ui.overwrite.as_ref().is_some_and(|save| {
            save.owner != owner || self.native_document_current(&save.document).is_none()
        }) {
            self.native_ui.overwrite = None;
            self.set_status("Save a Copy cancelled because its document changed");
        }
    }
    fn apply_native_choice(&mut self, owner: Owner, target: Target, path: PathBuf) {
        if owner != self.native_owner() {
            return;
        }
        match target {
            Target::File => {
                if path.is_file() {
                    if self.project.is_none() {
                        if let Some(parent) = path.parent() {
                            self.open_project(parent);
                        }
                    }
                    self.open_file(&path);
                    self.focus_editor = true;
                } else {
                    self.set_status("The selected file is no longer available");
                }
            }
            Target::Project => {
                if path.is_dir() {
                    self.request_open_project(&path);
                } else {
                    self.set_status("The selected project folder is no longer available");
                }
            }
            Target::Copy(document) => self.prepare_save_copy(owner, document, path),
        }
    }
    fn prepare_save_copy(&mut self, owner: Owner, document: DocumentOwner, path: PathBuf) {
        let Some(index) = self.native_document_current(&document) else {
            self.set_status("Save a Copy cancelled because its document changed");
            return;
        };
        if !path.is_absolute() || path.is_dir() {
            self.set_status("Choose an absolute file path for the copy");
            return;
        }
        let normalized = rustpy_core::vfs::normalize_path(&path);
        if self.tabs.iter().any(|tab| {
            tab.state
                .path
                .as_ref()
                .is_some_and(|source| rustpy_core::vfs::normalize_path(source) == normalized)
        }) {
            self.set_status(
                "The destination is open in an editor; choose another file or use Save",
            );
            return;
        }
        let bytes = match &document.notebook {
            Some(text) => text.as_bytes().to_vec(),
            None => match self.tabs[index]
                .state
                .doc
                .to_bytes_with_policy(rustpy_text::SourcePolicy::for_path(&path))
            {
                Ok(bytes) => bytes,
                Err(error) => {
                    self.set_status(format!("Cannot encode the copy: {error}"));
                    return;
                }
            },
        };
        let expected = match self.file_vfs().read(&path, self.editor_read_limits()) {
            Ok(snapshot) => Some(snapshot.revision),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => {
                self.set_status(format!("Cannot inspect the copy destination: {error}"));
                return;
            }
        };
        let save = Overwrite {
            owner,
            document,
            path,
            bytes,
            expected,
        };
        if expected.is_some() {
            self.native_ui.overwrite = Some(save);
        } else {
            self.write_save_copy(save);
        }
    }
    fn write_save_copy(&mut self, save: Overwrite) {
        if save.owner != self.native_owner()
            || self.native_document_current(&save.document).is_none()
        {
            self.set_status("Save a Copy cancelled because its document changed");
            return;
        }
        match self.checked_editor_write(&save.path, &save.bytes, save.expected) {
            Ok(_) => {
                self.refresh_project_files();
                self.set_status(format!("Saved copy to {}", save.path.display()));
            }
            Err(error) => self.set_status(format!("Copy was not saved: {error}")),
        }
    }
    pub(super) fn native_ui_windows(&mut self, ctx: &egui::Context) {
        if !matches!(self.dialog, Dialog::None) {
            return;
        }
        if let Some(mut state) = self.native_ui.browser.take() {
            let mut open = true;
            let mut chosen = None;
            let mut cancelled = false;
            let title = match state.target {
                Target::File => "Open File",
                Target::Project => "Open Project Folder",
                Target::Copy(_) => "Save a Copy",
            };
            egui::Window::new(title)
                .open(&mut open)
                .default_width(680.)
                .show_modal(ctx, |ui| {
                    if let Some(error) = &state.error {
                        ui.colored_label(self.theme.warning, error);
                    }
                    let selection = state.browser.ui(
                        ui,
                        &self.theme,
                        matches!(state.target, Target::File),
                        340.,
                    );
                    if matches!(state.target, Target::File) {
                        chosen = selection;
                    }
                    if matches!(state.target, Target::Copy(_)) {
                        ui.horizontal(|ui| {
                            ui.label("File name");
                            ui.text_edit_singleline(&mut state.name);
                        });
                    }
                    ui.horizontal(|ui| {
                        if ui.button("Cancel").clicked() {
                            cancelled = true;
                        }
                        if ui
                            .button(if matches!(state.target, Target::Copy(_)) {
                                "Save Copy"
                            } else {
                                "Open"
                            })
                            .clicked()
                        {
                            chosen = match &state.target {
                                Target::File => {
                                    state.browser.selected.clone().filter(|path| path.is_file())
                                }
                                Target::Project => Some(
                                    state
                                        .browser
                                        .selected
                                        .clone()
                                        .filter(|path| path.is_dir())
                                        .unwrap_or_else(|| state.browser.dir.clone()),
                                ),
                                Target::Copy(_) => {
                                    if valid_filename(&state.name) {
                                        Some(state.browser.dir.join(&state.name))
                                    } else {
                                        state.error = Some(
                                            "Enter a file name without directory separators".into(),
                                        );
                                        None
                                    }
                                }
                            };
                        }
                    });
                });
            if let Some(path) = chosen {
                self.apply_native_choice(state.owner, state.target, path);
            } else if open && !cancelled {
                self.native_ui.browser = Some(state);
            }
        }
        if let Some(save) = self.native_ui.overwrite.take() {
            let mut open = true;
            let mut confirm = false;
            let mut cancel = false;
            egui::Window::new("Replace existing file?").open(&mut open).collapsible(false).show_modal(ctx,|ui|{
                ui.label(format!("{} already exists. Replace it with a copy of the captured editor contents?",save.path.display()));
                ui.horizontal(|ui|{if ui.button("Cancel").clicked(){cancel=true;}
if ui.button("Replace").clicked(){confirm=true;}});
            });
            if confirm {
                self.write_save_copy(save);
            } else if open && !cancel {
                self.native_ui.overwrite = Some(save);
            }
        }
    }
    pub(super) fn native_project_window_merge_available(&self) -> bool {
        crate::launch::native_project_windows::available() && self.native_project_binding.is_some()
    }
    pub(super) fn merge_all_project_windows_native(&mut self) {
        if let Err(error) =
            crate::launch::native_project_windows::merge(self.native_project_binding.as_ref())
        {
            self.set_status(error);
        }
    }
}
fn valid_filename(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && !name.contains(['/', '\\', '\0'])
        && Path::new(name).file_name().is_some_and(|leaf| leaf == name)
}
#[cfg(test)]
mod fixtures {
    use super::*;
    #[test]
    fn save_copy_names_cannot_escape_the_chosen_folder() {
        assert!(valid_filename("notes ' $(literal).py"));
        assert!(!valid_filename("../outside.py"));
        assert!(!valid_filename("nested/file.py"));
        assert!(!valid_filename(".."));
        assert!(!valid_filename("x\0.py"));
    }
    #[test]
    fn confirmed_copy_cannot_overwrite_a_later_disk_revision_or_changed_editor() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source.txt");
        let target = directory.path().join("copy.txt");
        std::fs::write(&source, "source contents").unwrap();
        std::fs::write(&target, "first revision").unwrap();
        let mut settings = Settings::default();
        settings.save_synchronization.backup_before_save = false;
        let mut app = IdeApp::new_welcome(settings);
        app.open_file(&source);
        let index = app.active.unwrap();
        let tab = &app.tabs[index];
        let captured = DocumentOwner {
            id: tab.state.doc.identity(),
            version: tab.state.doc.version(),
            path: tab.state.path.clone(),
            notebook: None,
        };
        app.prepare_save_copy(app.native_owner(), captured, target.clone());
        let save = app
            .native_ui
            .overwrite
            .take()
            .expect("Existing destination requires explicit confirmation");
        std::fs::write(&target, "external revision after confirmation was shown").unwrap();
        app.write_save_copy(save);
        assert_eq!(
            std::fs::read_to_string(&target).unwrap(),
            "external revision after confirmation was shown"
        );
        assert_eq!(app.tabs[index].state.path.as_ref(), Some(&source));
        assert_eq!(app.tabs[index].state.doc.text(), "source contents");
    }
    #[test]
    fn stale_workspace_and_document_never_write_or_open_from_a_completed_request() {
        let mut app = IdeApp::new_welcome(Settings::default());
        let before = app.native_owner();
        app.file_session += 1;
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("copy.txt");
        app.apply_native_choice(before, Target::File, path.clone());
        assert!(app.tabs.is_empty());
        let editor = crate::editor::EditorState::from_text("old", Language::PlainText);
        let captured = DocumentOwner {
            id: editor.doc.identity(),
            version: editor.doc.version(),
            path: None,
            notebook: None,
        };
        app.prepare_save_copy(app.native_owner(), captured, path.clone());
        assert!(!path.exists());
    }
}

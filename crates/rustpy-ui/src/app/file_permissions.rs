//! Checked OS file-attribute changes, separate from library/viewer protection.
//! A prompt is bound to one document, path, file identity and observed attribute.
//! Successful chmod never marks saved or alters editor text/history/selections.

use std::fs::{File, Metadata, OpenOptions, Permissions};
use std::io;
use std::path::{Path, PathBuf};

use crate::editor::{doc_render, EditorState, Language};
use rustpy_text::DocumentId;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(super) enum Protection {
    Editable,
    /// Reader-mode protection may be released by an explicit confirmation.
    Library,
    /// Bundled stubs and image/hex viewers cannot become text editors.
    Permanent,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
struct Attribute {
    identity: (u64, u64),
    bits: u32,
    readonly: bool,
}

#[derive(Debug)]
pub(super) struct WritableRequest {
    document: DocumentId,
    path: PathBuf,
    observed: Attribute,
    protection: Protection,
    focus_requested: bool,
}

impl WritableRequest {
    pub(super) fn path(&self) -> &Path {
        &self.path
    }
    pub(super) fn take_initial_focus(&mut self) -> bool {
        !std::mem::replace(&mut self.focus_requested, true)
    }
}

pub(super) struct FilePermissionState {
    document: DocumentId,
    path: PathBuf,
    identity: (u64, u64),
    protection: Protection,
    /// Write bits removed by our toggle, never unrelated execute/read bits.
    restore_write_bits: Option<u32>,
    last_model_read_only: bool,
    manual_read_only: bool,
}

impl FilePermissionState {
    pub(super) fn open(editor: &mut EditorState, protection: Protection) -> io::Result<Self> {
        let path = editor.path.clone().ok_or_else(|| invalid("No disk file"))?;
        if path.starts_with("<typeshed>") {
            return Err(invalid("Bundled stubs have no mutable disk attribute"));
        }
        let (file, metadata) = open_regular(&path, false)?;
        let attribute = attribute(&file, &metadata)?;
        let manual_read_only =
            editor.doc.is_read_only() && !attribute.readonly && protection == Protection::Editable;
        let effective =
            attribute.readonly || protection != Protection::Editable || manual_read_only;
        doc_render::set_file_read_only(
            editor,
            attribute.readonly || protection == Protection::Permanent || manual_read_only,
        );
        editor.doc.set_read_only(effective);
        Ok(Self {
            document: editor.doc.identity(),
            path,
            identity: attribute.identity,
            protection,
            restore_write_bits: None,
            last_model_read_only: effective,
            manual_read_only,
        })
    }

    /// Called on focus/external refresh and before an edit/save/attribute action.
    /// A failed probe retains the previous model; replacement requires rebinding.
    pub(super) fn refresh(&mut self, editor: &mut EditorState) -> io::Result<bool> {
        let (_, _, attribute) = self.checked_file(editor, false)?;
        if editor.doc.is_read_only()
            && !self.last_model_read_only
            && self.protection == Protection::Editable
        {
            self.manual_read_only = true;
        }
        self.apply_model(editor, attribute.readonly);
        Ok(attribute.readonly)
    }

    /// Reader-mode settings may change protection independently of the OS flag.
    pub(super) fn set_protection(&mut self, protection: Protection) {
        self.protection = protection;
    }

    /// Lock an editable regular file, preserving every non-write permission bit.
    pub(super) fn set_read_only(&mut self, editor: &mut EditorState) -> io::Result<()> {
        if self.protection != Protection::Editable {
            return Err(denied(
                "Protected documents cannot toggle their disk attribute",
            ));
        }
        let (file, metadata, observed) = self.checked_file(editor, true)?;
        let permissions = permissions_for(&metadata, true, None);
        file.set_permissions(permissions)?;
        self.verify_change(&file, true)?;
        if !observed.readonly {
            self.restore_write_bits = Some(observed.bits & 0o222);
        }
        self.apply_model(editor, true);
        Ok(())
    }

    /// Produces a confirmation subject; this does not change permissions or
    /// unlock library protection. Cancel simply drops the returned request.
    pub(super) fn prepare_make_writable(
        &self,
        editor: &EditorState,
    ) -> io::Result<WritableRequest> {
        if self.protection == Protection::Permanent || self.manual_read_only {
            return Err(denied(
                "This viewer or bundled stub cannot be made writable",
            ));
        }
        if self.protection == Protection::Library && editor.language != Language::Python {
            return Err(denied("Only a real Python library source can be unlocked"));
        }
        let (_, _, observed) = self.checked_file(editor, false)?;
        Ok(WritableRequest {
            document: self.document,
            path: self.path.clone(),
            observed,
            protection: self.protection,
            focus_requested: false,
        })
    }

    pub(super) fn confirm_make_writable(
        &mut self,
        editor: &mut EditorState,
        request: WritableRequest,
    ) -> io::Result<()> {
        let (file, metadata, observed) = self.checked_file(editor, true)?;
        if request.document != self.document
            || request.path != self.path
            || request.observed != observed
            || request.protection != self.protection
            || self.protection == Protection::Permanent
            || self.manual_read_only
        {
            return Err(invalid(
                "The make-writable prompt is stale; inspect the file again",
            ));
        }
        if self.protection == Protection::Library && editor.language != Language::Python {
            return Err(denied("Only a real Python library source can be unlocked"));
        }
        if observed.readonly {
            file.set_permissions(permissions_for(&metadata, false, self.restore_write_bits))?;
            self.verify_change(&file, false)?;
        }
        doc_render::set_file_read_only(editor, false);
        if self.protection == Protection::Library {
            // Keep existing reader-mode 'unlocked' ownership, rather than only
            // clearing Document's bit and being relocked on the next frame.
            editor.doc.set_read_only(true);
            if !doc_render::make_writable(editor) {
                return Err(denied("The library source cannot be unlocked"));
            }
            self.protection = Protection::Editable;
        }
        self.apply_model(editor, false);
        self.restore_write_bits = None;
        Ok(())
    }

    fn apply_model(&mut self, editor: &mut EditorState, disk_read_only: bool) {
        let external =
            disk_read_only || self.protection == Protection::Permanent || self.manual_read_only;
        doc_render::set_file_read_only(editor, external);
        let effective = external || self.protection == Protection::Library;
        editor.doc.set_read_only(effective);
        self.last_model_read_only = effective;
    }

    fn bound_to(&self, editor: &EditorState) -> bool {
        self.document == editor.doc.identity() && editor.path.as_ref() == Some(&self.path)
    }

    fn checked_file(
        &self,
        editor: &EditorState,
        change: bool,
    ) -> io::Result<(File, Metadata, Attribute)> {
        if editor.doc.identity() != self.document || editor.path.as_ref() != Some(&self.path) {
            return Err(invalid("Document or path changed"));
        }
        let (file, metadata) = open_regular(&self.path, change)?;
        let attribute = attribute(&file, &metadata)?;
        if attribute.identity != self.identity {
            return Err(invalid("Disk file was replaced"));
        }
        Ok((file, metadata, attribute))
    }

    fn verify_change(&self, file: &File, readonly: bool) -> io::Result<()> {
        let metadata = file.metadata()?;
        let actual = attribute(file, &metadata)?;
        // The handle owns the file throughout mutation; recheck the named path
        // too, so a Unix rename cannot make the editor claim a new file changed.
        let (named, named_metadata) = open_regular(&self.path, false)?;
        let named_attribute = attribute(&named, &named_metadata)?;
        if actual.identity != self.identity
            || named_attribute.identity != self.identity
            || actual.readonly != readonly
        {
            return Err(invalid(
                "File identity or permission changed during the operation",
            ));
        }
        Ok(())
    }
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}
fn denied(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, message)
}

fn open_regular(path: &Path, change: bool) -> io::Result<(File, Metadata)> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // O_NONBLOCK also makes a replaced FIFO refusal bounded.
        let _ = change;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
        // Do not follow reparse points; deny delete/replace only for the short
        // operation, not the tab lifetime (which would break Windows Rename).
        options.custom_flags(0x0020_0000).share_mode(0x1 | 0x2);
        // Attribute mutation needs FILE_WRITE_ATTRIBUTES, not content-write
        // access (which the READONLY flag itself denies).
        if change {
            options.access_mode(0x80 | 0x100);
        }
        let metadata = std::fs::symlink_metadata(path)?;
        if metadata.file_attributes() & 0x400 != 0 {
            return Err(invalid("Reparse points have no mutable editor attribute"));
        }
    }
    let file = options.open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(invalid(
            "Only a regular file has a mutable editor attribute",
        ));
    }
    Ok((file, metadata))
}

fn attribute(file: &File, metadata: &Metadata) -> io::Result<Attribute> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let _ = file;
        Ok(Attribute {
            identity: (metadata.dev(), metadata.ino()),
            bits: metadata.permissions().mode(),
            readonly: metadata.permissions().readonly(),
        })
    }
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        #[repr(C)]
        struct FileInformation {
            attributes: u32,
            creation: [u32; 2],
            access: [u32; 2],
            write: [u32; 2],
            volume: u32,
            size_high: u32,
            size_low: u32,
            links: u32,
            index_high: u32,
            index_low: u32,
        }
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn GetFileInformationByHandle(
                handle: *mut std::ffi::c_void,
                info: *mut FileInformation,
            ) -> i32;
        }
        let mut info = std::mem::MaybeUninit::<FileInformation>::uninit();
        if unsafe { GetFileInformationByHandle(file.as_raw_handle(), info.as_mut_ptr()) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let info = unsafe { info.assume_init() };
        Ok(Attribute {
            identity: (
                u64::from(info.volume),
                (u64::from(info.index_high) << 32) | u64::from(info.index_low),
            ),
            bits: info.attributes,
            readonly: metadata.permissions().readonly(),
        })
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (file, metadata);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "File attribute identity is unsupported on this platform",
        ))
    }
}

fn permissions_for(metadata: &Metadata, readonly: bool, restore: Option<u32>) -> Permissions {
    let mut permissions = metadata.permissions();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = permissions.mode();
        permissions.set_mode(if readonly {
            mode & !0o222
        } else {
            mode | restore.filter(|bits| *bits != 0).unwrap_or(0o200)
        });
    }
    #[cfg(not(unix))]
    {
        let _ = restore;
        permissions.set_readonly(readonly);
    }
    permissions
}

impl super::IdeApp {
    fn permission_protection(&self, index: usize) -> Protection {
        let tab = &self.tabs[index];
        if tab.image.is_some()
            || self.permission_viewers.contains(&tab.state.doc.identity())
            || tab
                .state
                .path
                .as_ref()
                .is_some_and(|p| p.starts_with("<typeshed>"))
            || self.is_large_file_placeholder(index)
            || u64::from(u32::from(tab.state.doc.len())) > super::background::READ_ONLY_FILE_BYTES
        {
            Protection::Permanent
        } else if doc_render::library_protected(&tab.state, &self.settings.editor) {
            Protection::Library
        } else {
            Protection::Editable
        }
    }

    /// Bind after an accepted open/reload/save. Only our accepted identity
    /// transition may discard a previous disk-derived guard; external replace
    /// is handled by the existing checked reload/conflict path instead.
    pub(super) fn bind_file_permissions(&mut self, index: usize) {
        let Some(tab) = self.tabs.get(index) else {
            return;
        };
        let Some(path) = tab.state.path.clone() else {
            return;
        };
        if path.starts_with("<typeshed>") || self.is_large_file_placeholder(index) {
            return;
        }
        let document = tab.state.doc.identity();
        let old_model_read_only = tab.state.doc.is_read_only();
        let old_file_read_only = doc_render::file_read_only(&tab.state);
        if let Some(project) = &self.project {
            doc_render::lock_library_file(
                &mut self.tabs[index].state,
                &project
                    .analysis
                    .resolver
                    .search_paths
                    .iter()
                    .chain(&project.analysis.resolver.stub_roots)
                    .cloned()
                    .collect::<Vec<_>>(),
                &project.analysis.resolver.roots,
                self.settings.editor.reader_mode.protect_library_sources,
            );
        }
        let protection = self.permission_protection(index);
        let old = self
            .file_permissions
            .iter()
            .find_map(|(id, state)| (state.path == path || *id == document).then_some(*id));
        let mut restored_bits = None;
        if let Some(old) = old {
            if let Some(previous) = self.file_permissions.remove(&old) {
                restored_bits = Some((previous.identity, previous.restore_write_bits));
                if !previous.manual_read_only && previous.protection != Protection::Permanent {
                    // Reload's existing compatibility policy preserves a bool.
                    // Remove only our old derived guard, then reprobe the new inode.
                    doc_render::set_file_read_only(&mut self.tabs[index].state, false);
                }
            }
        }
        match FilePermissionState::open(&mut self.tabs[index].state, protection) {
            Ok(mut state) => {
                if let Some((identity, bits)) = restored_bits {
                    if state.identity == identity {
                        state.restore_write_bits = bits;
                    }
                }
                self.file_permissions.insert(document, state);
            }
            Err(error) => {
                doc_render::set_file_read_only(&mut self.tabs[index].state, old_file_read_only);
                self.tabs[index]
                    .state
                    .doc
                    .set_read_only(old_model_read_only);
                if error.kind() != io::ErrorKind::NotFound {
                    self.set_status(format!("Cannot inspect file permissions: {error}"));
                }
            }
        }
        self.prune_file_permissions();
    }

    pub(super) fn prune_file_permissions(&mut self) {
        self.file_permissions
            .retain(|id, _| self.tabs.iter().any(|t| t.state.doc.identity() == *id));
        self.permission_viewers
            .retain(|id| self.tabs.iter().any(|t| t.state.doc.identity() == *id));
    }

    pub(super) fn refresh_file_permissions(&mut self, index: usize) -> bool {
        let Some(tab) = self.tabs.get(index) else {
            return false;
        };
        let Some(path) = tab.state.path.as_ref() else {
            return true;
        };
        if path.starts_with("<typeshed>")
            || self.is_large_file_placeholder(index)
            || tab.image.is_some()
        {
            return true;
        }
        // Keep explicitly acknowledges a missing disk baseline. Preserve every
        // model guard; the existing Save pipeline still checks absence before
        // its checked write and binds the resulting new inode afterward.
        if self.disk_revisions.get(path) == Some(&None)
            && std::fs::symlink_metadata(path)
                .is_err_and(|error| error.kind() == io::ErrorKind::NotFound)
        {
            return true;
        }
        let id = tab.state.doc.identity();
        let protection = self.permission_protection(index);
        if self
            .file_permissions
            .get(&id)
            .is_none_or(|state| !state.bound_to(&tab.state))
        {
            // Missing unsaved files have no attribute yet. Preserve existing
            // model guards and let the normal save pipeline create them.
            if !path.exists() {
                return !self.file_permissions.contains_key(&id);
            }
            self.bind_file_permissions(index);
        }
        let Some(state) = self.file_permissions.get_mut(&id) else {
            return false;
        };
        state.set_protection(protection);
        match state.refresh(&mut self.tabs[index].state) {
            Ok(_) => true,
            Err(error) => {
                self.set_status(format!("Cannot refresh file permissions: {error}"));
                false
            }
        }
    }

    pub(super) fn request_make_writable(&mut self, index: usize) {
        if self.coordinated_quit.is_some() || !matches!(self.dialog, super::Dialog::None) {
            self.set_status("Close the current confirmation before changing file permissions");
            return;
        }
        if !self.refresh_file_permissions(index) {
            return;
        }
        let id = self.tabs[index].state.doc.identity();
        let Some(state) = self.file_permissions.get(&id) else {
            self.set_status("This viewer or unsaved file has no writable disk attribute");
            return;
        };
        match state.prepare_make_writable(&self.tabs[index].state) {
            Ok(request) => {
                self.dialog = super::Dialog::MakeWritable {
                    document: id,
                    request,
                }
            }
            Err(error) => self.set_status(format!("Cannot make writable: {error}")),
        }
    }

    pub(super) fn toggle_file_read_only(&mut self, index: usize) {
        if self.coordinated_quit.is_some() || !matches!(self.dialog, super::Dialog::None) {
            self.set_status("Close the current confirmation before changing file permissions");
            return;
        }
        if !self.refresh_file_permissions(index) {
            return;
        }
        if self.tabs[index].state.doc.is_read_only() {
            self.request_make_writable(index);
            return;
        }
        let id = self.tabs[index].state.doc.identity();
        let Some(state) = self.file_permissions.get_mut(&id) else {
            self.set_status("This file has no mutable disk attribute");
            return;
        };
        match state.set_read_only(&mut self.tabs[index].state) {
            Ok(()) => self.set_status("File attribute changed to read-only"),
            Err(error) => self.set_status(format!("Cannot change file permissions: {error}")),
        }
    }

    /// A stale index is never used: the prompt identifies the original document.
    pub(super) fn answer_make_writable(
        &mut self,
        document: DocumentId,
        request: WritableRequest,
        confirm: bool,
    ) {
        if !confirm {
            return;
        }
        let Some(index) = self
            .tabs
            .iter()
            .position(|t| t.state.doc.identity() == document)
        else {
            self.set_status("Make writable cancelled: the original document is no longer open");
            return;
        };
        let protection = self.permission_protection(index);
        let Some(state) = self.file_permissions.get_mut(&document) else {
            self.set_status("Make writable cancelled: the file binding changed");
            return;
        };
        state.set_protection(protection);
        match state.confirm_make_writable(&mut self.tabs[index].state, request) {
            Ok(()) => {
                self.set_status("File is writable");
                self.focus_editor = true;
            }
            Err(error) => self.set_status(format!("Cannot make writable: {error}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustpy_text::{Document, Edit, EditGroup, TextSize};

    fn editor(path: &Path) -> EditorState {
        EditorState::new(Document::new("π🙂\n"), Some(path.to_path_buf()))
    }

    struct Directory(tempfile::TempDir);
    impl Directory {
        fn path(&self) -> &Path {
            self.0.path()
        }
    }
    impl Drop for Directory {
        fn drop(&mut self) {
            // Windows cannot remove READONLY fixtures. Cleanup never follows
            // the symlink refusal fixture and modifies only owned regular files.
            if let Ok(entries) = std::fs::read_dir(self.path()) {
                for entry in entries.flatten() {
                    if let Ok(metadata) = std::fs::symlink_metadata(entry.path()) {
                        if metadata.is_file() && !metadata.file_type().is_symlink() {
                            let _ = std::fs::set_permissions(
                                entry.path(),
                                permissions_for(&metadata, false, None),
                            );
                        }
                    }
                }
            }
        }
    }
    fn directory() -> Directory {
        Directory(tempfile::tempdir().unwrap())
    }

    #[test]
    fn toggle_changes_real_attribute_without_editing_saving_or_moving_carets() {
        let directory = directory();
        let path = directory.path().join("file.py");
        std::fs::write(&path, "π🙂\n").unwrap();
        let mut editor = editor(&path);
        editor
            .doc
            .edit(
                vec![Edit::insert(TextSize::new(0), "# changed\n")],
                &[],
                &[],
                EditGroup::Separate,
                None,
            )
            .unwrap();
        let before = (
            editor.doc.text(),
            editor.doc.version(),
            editor.doc.current_history(),
            editor.doc.is_modified(),
            editor.selections(),
        );
        let mut state = FilePermissionState::open(&mut editor, Protection::Editable).unwrap();
        state.set_read_only(&mut editor).unwrap();
        assert!(std::fs::metadata(&path).unwrap().permissions().readonly());
        assert!(editor.doc.is_read_only());
        let prompt = state.prepare_make_writable(&editor).unwrap();
        state.confirm_make_writable(&mut editor, prompt).unwrap();
        assert!(!std::fs::metadata(&path).unwrap().permissions().readonly());
        assert!(!editor.doc.is_read_only());
        assert_eq!(
            (
                editor.doc.text(),
                editor.doc.version(),
                editor.doc.current_history(),
                editor.doc.is_modified(),
                editor.selections()
            ),
            before
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "π🙂\n");
    }

    #[cfg(unix)]
    #[test]
    fn unix_toggle_restores_only_original_write_bits_and_keeps_execute_and_special_bits() {
        use std::os::unix::fs::PermissionsExt;
        let directory = directory();
        let path = directory.path().join("script.py");
        std::fs::write(&path, "pass\n").unwrap();
        std::fs::set_permissions(&path, Permissions::from_mode(0o1754)).unwrap();
        let original = std::fs::metadata(&path).unwrap().permissions().mode();
        let mut editor = editor(&path);
        let mut state = FilePermissionState::open(&mut editor, Protection::Editable).unwrap();
        state.set_read_only(&mut editor).unwrap();
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode(),
            original & !0o222
        );
        let prompt = state.prepare_make_writable(&editor).unwrap();
        state.confirm_make_writable(&mut editor, prompt).unwrap();
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode(),
            original
        );
    }

    #[cfg(unix)]
    #[test]
    fn initially_readonly_file_gets_owner_write_not_group_or_world_write() {
        use std::os::unix::fs::PermissionsExt;
        let directory = directory();
        let path = directory.path().join("file.py");
        std::fs::write(&path, "pass\n").unwrap();
        std::fs::set_permissions(&path, Permissions::from_mode(0o444)).unwrap();
        let mut editor = editor(&path);
        let mut state = FilePermissionState::open(&mut editor, Protection::Editable).unwrap();
        assert!(editor.doc.is_read_only());
        let prompt = state.prepare_make_writable(&editor).unwrap();
        state.confirm_make_writable(&mut editor, prompt).unwrap();
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o644
        );
    }

    #[test]
    fn stale_prompt_and_foreign_document_refuse_without_clearing_readonly() {
        let directory = directory();
        let path = directory.path().join("file.py");
        std::fs::write(&path, "pass\n").unwrap();
        let mut editor = editor(&path);
        let mut state = FilePermissionState::open(&mut editor, Protection::Editable).unwrap();
        state.set_read_only(&mut editor).unwrap();
        let prompt = state.prepare_make_writable(&editor).unwrap();
        let permissions = permissions_for(&std::fs::metadata(&path).unwrap(), false, None);
        std::fs::set_permissions(&path, permissions).unwrap();
        assert!(state.confirm_make_writable(&mut editor, prompt).is_err());
        assert!(editor.doc.is_read_only());
        let mut foreign = super::tests::editor(&path);
        assert!(state.refresh(&mut foreign).is_err());
        assert!(!foreign.doc.is_read_only());
    }

    #[test]
    fn missing_file_retains_model_and_prompt_cancel_does_nothing() {
        let directory = directory();
        let path = directory.path().join("file.py");
        std::fs::write(&path, "pass\n").unwrap();
        let mut editor = editor(&path);
        let mut state = FilePermissionState::open(&mut editor, Protection::Editable).unwrap();
        state.set_read_only(&mut editor).unwrap();
        drop(state.prepare_make_writable(&editor).unwrap());
        assert!(editor.doc.is_read_only());
        assert!(std::fs::metadata(&path).unwrap().permissions().readonly());
        // Windows refuses deletion of a READONLY file. Deliberately remove the
        // disk attribute while retaining the document's refused-edit state.
        std::fs::set_permissions(
            &path,
            permissions_for(&std::fs::metadata(&path).unwrap(), false, None),
        )
        .unwrap();
        std::fs::remove_file(&path).unwrap();
        assert!(state.refresh(&mut editor).is_err());
        assert!(state.set_read_only(&mut editor).is_err());
        assert!(editor.doc.is_read_only());
    }

    #[test]
    fn external_attribute_refresh_and_permanent_protection_remain_separate() {
        let directory = directory();
        let path = directory.path().join("file.py");
        std::fs::write(&path, "pass\n").unwrap();
        let mut editor = editor(&path);
        let mut state = FilePermissionState::open(&mut editor, Protection::Editable).unwrap();
        let mut permissions = std::fs::metadata(&path).unwrap().permissions();
        permissions.set_readonly(true);
        std::fs::set_permissions(&path, permissions).unwrap();
        assert!(state.refresh(&mut editor).unwrap());
        assert!(editor.doc.is_read_only());
        state.set_protection(Protection::Permanent);
        assert!(state.prepare_make_writable(&editor).is_err());
        assert!(state.set_read_only(&mut editor).is_err());
        assert!(std::fs::metadata(&path).unwrap().permissions().readonly());
    }

    #[test]
    fn library_confirmation_keeps_reader_unlock_policy_and_stubs_never_unlock() {
        let directory = directory();
        let path = directory.path().join("library.py");
        std::fs::write(&path, "pass\n").unwrap();
        let mut editor = editor(&path);
        let mut state = FilePermissionState::open(&mut editor, Protection::Library).unwrap();
        assert!(editor.doc.is_read_only());
        assert!(!std::fs::metadata(&path).unwrap().permissions().readonly());
        let prompt = state.prepare_make_writable(&editor).unwrap();
        state.confirm_make_writable(&mut editor, prompt).unwrap();
        assert!(!state.refresh(&mut editor).unwrap());
        assert!(!editor.doc.is_read_only());
        doc_render::lock_library_file(&mut editor, &[directory.path().to_path_buf()], &[], true);
        assert!(
            !editor.doc.is_read_only(),
            "interpreter refresh respects the explicit library unlock"
        );
        let mut stub = EditorState::new(
            Document::new("..."),
            Some(PathBuf::from("<typeshed>/x.pyi")),
        );
        stub.doc.set_read_only(true);
        assert!(FilePermissionState::open(&mut stub, Protection::Permanent).is_err());
        assert!(stub.doc.is_read_only());
    }

    #[cfg(unix)]
    #[test]
    fn replaced_identity_directory_and_symlink_are_refused() {
        use std::os::unix::fs::symlink;
        let directory = directory();
        let path = directory.path().join("file.py");
        std::fs::write(&path, "pass\n").unwrap();
        let mut editor = editor(&path);
        let mut state = FilePermissionState::open(&mut editor, Protection::Editable).unwrap();
        let replacement = directory.path().join("replacement");
        std::fs::write(&replacement, "other\n").unwrap();
        std::fs::rename(&replacement, &path).unwrap();
        assert!(state.set_read_only(&mut editor).is_err());
        assert!(!editor.doc.is_read_only());
        assert!(!std::fs::metadata(&path).unwrap().permissions().readonly());
        let link = directory.path().join("link.py");
        symlink(&path, &link).unwrap();
        assert!(
            FilePermissionState::open(&mut super::tests::editor(&link), Protection::Editable)
                .is_err()
        );
        assert!(FilePermissionState::open(
            &mut super::tests::editor(directory.path()),
            Protection::Editable
        )
        .is_err());
    }
}

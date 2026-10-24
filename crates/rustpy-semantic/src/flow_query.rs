//! Per-Inferer reusable immutable terminal authority. Registered under flow;
//! source and dependency checks remain identical to uncached db-aware flow.
use super::*;
use crate::db::Analysis;

pub(crate) const MAX_AUTHORITY_CACHE_BYTES: usize = 4 * 1024 * 1024;
pub(crate) const MAX_AUTHORITY_CACHE_FILES: usize = 4;

pub(crate) struct CapturedAuthority {
    file: Arc<FileInfo>,
    epoch: u64,
    authority: terminal::Authority,
    bytes: usize,
}
impl CapturedAuthority {
    pub(crate) fn file_id(&self) -> rustpy_core::FileId {
        self.file.id
    }
    pub(crate) fn bytes(&self) -> usize {
        self.bytes
    }
    pub(crate) fn matches(&self, db: &Analysis, file: &Arc<FileInfo>, epoch: u64) -> bool {
        self.epoch == epoch && Arc::ptr_eq(&self.file, file) && self.current(db)
    }
    fn current(&self, db: &Analysis) -> bool {
        db.revision_excluding_file(&self.file) == self.epoch
            && db
                .file_recently_checked(self.file.id)
                .is_some_and(|current| Arc::ptr_eq(&current, &self.file))
            && self.authority.is_current(db)
            && db.revision_excluding_file(&self.file) == self.epoch
    }
}

pub(crate) fn capture_authority(
    db: &Analysis,
    file: &Arc<FileInfo>,
) -> Option<Arc<CapturedAuthority>> {
    let epoch = db.revision_excluding_file(file);
    if !db
        .file_recently_checked(file.id)
        .is_some_and(|current| Arc::ptr_eq(&current, file))
    {
        return None;
    }
    let authority = terminal::Authority::captured(db, file);
    let bytes = authority.retained_bytes(file);
    let captured = Arc::new(CapturedAuthority {
        file: file.clone(),
        epoch,
        authority,
        bytes,
    });
    captured.current(db).then_some(captured)
}

pub(crate) fn flow_with_authority(
    db: &Analysis,
    file: &FileInfo,
    scope: ScopeId,
    captured: &CapturedAuthority,
) -> Arc<FlowInfo> {
    if !std::ptr::eq(file, captured.file.as_ref()) || !captured.current(db) {
        return flow_of(file, scope);
    }
    let result = Arc::new(analyze(file, scope, captured.authority.clone()));
    if !captured.current(db) {
        return flow_of(file, scope);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::infer::Inferer;

    #[test]
    fn one_source_capture_is_reused_across_scopes_and_retired_after_dependency_edit() {
        let directory = tempfile::TempDir::with_prefix("rustpy-terminal-authority-cache").unwrap();
        let path = directory.path().join("main.py");
        std::fs::write(
            &path,
            "import sys\ndef first():\n    sys.exit()\ndef second():\n    sys.exit()\n",
        )
        .unwrap();
        let db = Analysis::for_project(directory.path());
        let file = db.file_for_path(&path).unwrap();
        let inferer = Inferer::new(&db);
        let functions: Vec<_> = file
            .parse
            .syntax_node()
            .descendants()
            .filter_map(ast::FuncDef::cast)
            .collect();
        let first = file
            .scopes
            .scope_for_node(functions[0].syntax().text_range())
            .unwrap();
        let second = file
            .scopes
            .scope_for_node(functions[1].syntax().text_range())
            .unwrap();
        assert_eq!(inferer.scope_flow(&file, first).terminal_calls.len(), 1);
        let captured = inferer.terminal_authority_cache.borrow()[0].clone();
        assert_eq!(inferer.scope_flow(&file, second).terminal_calls.len(), 1);
        assert_eq!(inferer.terminal_authority_cache.borrow().len(), 1);
        assert!(Arc::ptr_eq(
            &captured,
            &inferer.terminal_authority_cache.borrow()[0]
        ));
        let frozen = db
            .snapshot(
                std::slice::from_ref(&path),
                &rustpy_core::CancellationToken::new(),
            )
            .unwrap();
        let frozen_file = frozen.analysis().file(file.id).unwrap();
        std::fs::write(
            directory.path().join("sys.py"),
            "def exit():\n    return None\n",
        )
        .unwrap();
        db.invalidate_modules();
        assert!(inferer.scope_flow(&file, second).terminal_calls.is_empty());
        assert!(!Arc::ptr_eq(
            &captured,
            &inferer.terminal_authority_cache.borrow()[0]
        ));
        assert_eq!(
            Inferer::new(frozen.analysis())
                .scope_flow(&frozen_file, second)
                .terminal_calls
                .len(),
            1
        );
    }

    #[test]
    fn capture_cache_has_bounded_file_count_and_retained_source_weight() {
        let directory =
            tempfile::TempDir::with_prefix("rustpy-terminal-authority-cache-limit").unwrap();
        let db = Analysis::for_project(directory.path());
        let inferer = Inferer::new(&db);
        let mut first = None;
        for index in 0..6 {
            let path = directory.path().join(format!("module{index}.py"));
            let id = db.set_file_text(&path, "marker=1\nmarker\n");
            let file = db.file(id).unwrap();
            if index == 0 {
                first = Some(file.id);
            }
            inferer.scope_flow(&file, ScopeId(0));
            let cache = inferer.terminal_authority_cache.borrow();
            assert!(cache.len() <= MAX_AUTHORITY_CACHE_FILES);
            assert!(
                cache.iter().map(|entry| entry.bytes()).sum::<usize>() <= MAX_AUTHORITY_CACHE_BYTES
            );
        }
        assert!(!inferer
            .terminal_authority_cache
            .borrow()
            .iter()
            .any(|entry| Some(entry.file_id()) == first));
    }
    #[test]
    fn aggregate_source_byte_budget_evicts_before_the_file_count_limit() {
        let directory =
            tempfile::TempDir::with_prefix("rustpy-terminal-authority-byte-limit").unwrap();
        let db = Analysis::for_project(directory.path());
        let inferer = Inferer::new(&db);
        let source = format!("{}marker=1\nmarker\n", "#padding\n".repeat(250_000));
        assert!(source.len() > MAX_AUTHORITY_CACHE_BYTES / 2);
        let first = db.set_file_text(&directory.path().join("first.py"), &source);
        inferer.scope_flow(&db.file(first).unwrap(), ScopeId(0));
        assert_eq!(inferer.terminal_authority_cache.borrow().len(), 1);
        let second = db.set_file_text(&directory.path().join("second.py"), &source);
        inferer.scope_flow(&db.file(second).unwrap(), ScopeId(0));
        let cache = inferer.terminal_authority_cache.borrow();
        assert_eq!(cache.len(), 1);
        assert_eq!(cache[0].file_id(), second);
        assert!(cache[0].bytes() <= MAX_AUTHORITY_CACHE_BYTES);
    }
}

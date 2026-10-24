//! Application-owned verified library-index context, never inferred from mtime.
use crate::native_skeletons::{Catalogue, InterpreterBinding, Source};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
};

#[derive(Clone, Debug, Default)]
pub struct VerifiedNativeIndex {
    pub trusted: bool,
    pub generation: u64,
    pub interpreter: Option<InterpreterBinding>,
    pub modules: BTreeMap<(String, PathBuf), String>,
}

#[derive(Clone, Debug, Default)]
pub struct NativeState {
    pub catalogue: Arc<Catalogue>,
    pub index: VerifiedNativeIndex,
}

impl NativeState {
    pub fn source(&self, name: &str, path: &Path) -> Option<Source> {
        let interpreter = self.index.interpreter.as_ref()?;
        let digest = self
            .index
            .modules
            .get(&(name.to_owned(), path.to_path_buf()))?;
        self.catalogue
            .lookup(
                self.index.trusted,
                self.index.generation,
                interpreter,
                name,
                path,
                digest,
            )
            .cloned()
    }
    pub fn virtual_path(&self, name: &str, path: &Path, source: &Source) -> Option<PathBuf> {
        let interpreter = self.index.interpreter.as_ref()?;
        let module_digest = self
            .index
            .modules
            .get(&(name.to_owned(), path.to_path_buf()))?;
        let revision: String = source
            .revision
            .0
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        Some(PathBuf::from(format!(
            "<typeshed>/@compiled/{}/{}/{module_digest}/{revision}/{name}.pyi",
            self.index.generation, interpreter.sha256
        )))
    }
}

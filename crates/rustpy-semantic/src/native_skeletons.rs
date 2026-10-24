//! Immutable trusted-interpreter skeletons. Lookup never executes Python.
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::PathBuf, sync::Arc};

const MAX_STUB: usize = 2 * 1024 * 1024;
const MAX_CATALOGUE: usize = 64 * 1024 * 1024;
const MAX_MODULES: usize = 4096;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InterpreterBinding {
    pub path: PathBuf,
    pub sha256: String,
    pub version: String,
    pub cache_tag: String,
    pub soabi: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Record {
    pub protocol: u32,
    pub module_name: String,
    pub module_path: PathBuf,
    pub module_sha256: String,
    pub interpreter: InterpreterBinding,
    pub pyi: String,
    /// Members not safely introspectable; these must never be silently invented.
    pub unsupported: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct Source {
    pub text: Arc<str>,
    pub revision: rustpy_core::vfs::ContentRevision,
    pub unsupported: Arc<[String]>,
}

#[derive(Clone, Debug, Default)]
pub struct Catalogue {
    interpreter: Option<InterpreterBinding>,
    /// Caller-owned trust/interpreter/library index generation.
    generation: u64,
    entries: BTreeMap<(String, PathBuf), (String, Source)>,
}

fn digest_valid(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}

impl Catalogue {
    /// Publish only after a trusted managed producer completed and its current
    /// interpreter/module identities were rechecked. No partial publication.
    pub fn from_records(
        trusted: bool,
        generation: u64,
        interpreter: InterpreterBinding,
        records: Vec<Record>,
    ) -> Result<Self, &'static str> {
        if !trusted {
            return Err("untrusted interpreter");
        }
        if !interpreter.path.is_absolute()
            || !digest_valid(&interpreter.sha256)
            || interpreter.version.is_empty()
            || interpreter.cache_tag.is_empty()
            || interpreter.version.len() > 1024
            || interpreter.cache_tag.len() > 1024
            || interpreter.soabi.len() > 1024
            || interpreter.path.as_os_str().len() > 8192
        {
            return Err("invalid interpreter binding");
        }
        if records.len() > MAX_MODULES {
            return Err("module budget");
        }
        let mut catalogue = Self {
            interpreter: Some(interpreter.clone()),
            generation,
            entries: BTreeMap::new(),
        };
        let mut bytes = 0usize;
        for record in records {
            if record.protocol != 1
                || record.interpreter != interpreter
                || !record.module_path.is_absolute()
                || !digest_valid(&record.module_sha256)
                || record.module_path.as_os_str().len() > 8192
            {
                return Err("record binding mismatch");
            }
            // Match the maintained resolver's qualified module-name policy.
            if record.module_name.len() > 256
                || !record
                    .module_name
                    .split('.')
                    .all(crate::module::is_identifier)
            {
                return Err("unsupported module name");
            }
            let file_name = record
                .module_path
                .file_name()
                .and_then(|x| x.to_str())
                .ok_or("non UTF-8 module basename")?;
            if crate::module::extension_module_name(file_name)
                != record.module_name.rsplit('.').next()
            {
                return Err("module basename mismatch");
            }
            if record.pyi.len() > MAX_STUB || record.unsupported.len() > MAX_MODULES {
                return Err("stub budget");
            }
            bytes = bytes
                .checked_add(record.pyi.len())
                .ok_or("catalogue budget")?;
            for reason in &record.unsupported {
                bytes = bytes.checked_add(reason.len()).ok_or("catalogue budget")?;
            }
            if bytes > MAX_CATALOGUE {
                return Err("catalogue budget");
            }
            if !rustpy_syntax::parse(&record.pyi).errors().is_empty() {
                return Err("invalid generated Python stub");
            }
            let revision = rustpy_core::vfs::ContentRevision::of(record.pyi.as_bytes());
            let source = Source {
                text: Arc::from(record.pyi),
                revision,
                unsupported: Arc::from(record.unsupported),
            };
            if catalogue
                .entries
                .insert(
                    (record.module_name, record.module_path),
                    (record.module_sha256, source),
                )
                .is_some()
            {
                return Err("duplicate skeleton");
            }
        }
        Ok(catalogue)
    }

    /// `current`/`module_sha256` are captured by the trusted library index's
    /// verified byte snapshot, not inferred from mtime or extension filename.
    pub fn lookup(
        &self,
        trusted: bool,
        generation: u64,
        current: &InterpreterBinding,
        name: &str,
        module_path: &std::path::Path,
        module_sha256: &str,
    ) -> Option<&Source> {
        if !trusted || generation != self.generation || self.interpreter.as_ref() != Some(current) {
            return None;
        }
        let (expected, source) = self
            .entries
            .get(&(name.to_owned(), module_path.to_path_buf()))?;
        (expected == module_sha256).then_some(source)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn interpreter() -> InterpreterBinding {
        InterpreterBinding {
            path: PathBuf::from("/trusted/python"),
            sha256: "a".repeat(64),
            version: "3.12.3".into(),
            cache_tag: "cpython-312".into(),
            soabi: "cpython-312-test".into(),
        }
    }
    fn record(extension: &str) -> Record {
        Record {
            protocol: 1,
            module_name: "native".into(),
            module_path: PathBuf::from(format!("/trusted/native{extension}")),
            module_sha256: "b".repeat(64),
            interpreter: interpreter(),
            pyi: "def answer() -> int: ...\n".into(),
            unsupported: vec![],
        }
    }
    #[test]
    #[cfg(unix)] // Unix fixture paths, tools or process semantics
    fn extension_sources_are_bound_to_exact_binary_and_interpreter() {
        for extension in [".cpython-312-test.so", ".pyd"] {
            let record = record(extension);
            let path = record.module_path.clone();
            let catalogue = Catalogue::from_records(true, 7, interpreter(), vec![record]).unwrap();
            let source = catalogue
                .lookup(true, 7, &interpreter(), "native", &path, &"b".repeat(64))
                .unwrap();
            assert_eq!(&*source.text, "def answer() -> int: ...\n");
            assert!(catalogue
                .lookup(true, 7, &interpreter(), "native", &path, &"c".repeat(64))
                .is_none());
            let mut changed = interpreter();
            changed.soabi = "another-ABI".into();
            assert!(catalogue
                .lookup(true, 7, &changed, "native", &path, &"b".repeat(64))
                .is_none());
        }
    }
    #[test]
    #[cfg(unix)] // Unix fixture paths, tools or process semantics
    fn qualified_extension_keeps_exact_package_and_binary_binding() {
        let mut rec = record(".pyd");
        rec.module_name = "package.native".into();
        let path = rec.module_path.clone();
        let catalogue = Catalogue::from_records(true, 7, interpreter(), vec![rec]).unwrap();
        assert!(catalogue
            .lookup(
                true,
                7,
                &interpreter(),
                "package.native",
                &path,
                &"b".repeat(64)
            )
            .is_some());
        assert!(catalogue
            .lookup(
                true,
                7,
                &interpreter(),
                "other.native",
                &path,
                &"b".repeat(64)
            )
            .is_none());
    }

    #[test]
    #[cfg(unix)] // Unix fixture paths, tools or process semantics
    fn trust_or_index_generation_changes_do_not_reuse_old_sources() {
        let rec = record(".so");
        let path = rec.module_path.clone();
        let old = Arc::new(Catalogue::from_records(true, 7, interpreter(), vec![rec]).unwrap());
        assert!(old
            .lookup(false, 7, &interpreter(), "native", &path, &"b".repeat(64))
            .is_none());
        assert!(old
            .lookup(true, 8, &interpreter(), "native", &path, &"b".repeat(64))
            .is_none());
        assert!(Catalogue::from_records(false, 7, interpreter(), vec![]).is_err());
        assert!(old
            .lookup(true, 7, &interpreter(), "native", &path, &"b".repeat(64))
            .is_some());
    }
    #[test]
    fn invalid_duplicate_and_oversized_stubs_never_partially_publish() {
        let mut invalid = record(".so");
        invalid.pyi = "def (".into();
        assert!(Catalogue::from_records(true, 1, interpreter(), vec![invalid]).is_err());
        let rec = record(".so");
        assert!(Catalogue::from_records(true, 1, interpreter(), vec![rec.clone(), rec]).is_err());
        let mut big = record(".so");
        big.pyi = " ".repeat(MAX_STUB + 1);
        assert!(Catalogue::from_records(true, 1, interpreter(), vec![big]).is_err());
    }
    #[test]
    #[cfg(unix)] // Unix fixture paths, tools or process semantics
    fn missing_catalogue_retains_truthful_fallback_and_unsupported_reasons() {
        assert!(Catalogue::default()
            .lookup(
                true,
                0,
                &interpreter(),
                "native",
                std::path::Path::new("/trusted/native.so"),
                &"b".repeat(64)
            )
            .is_none());
        let mut rec = record(".so");
        rec.unsupported
            .push("signature unavailable for native.answer".into());
        let catalogue = Catalogue::from_records(true, 1, interpreter(), vec![rec]).unwrap();
        assert_eq!(
            &*catalogue
                .lookup(
                    true,
                    1,
                    &interpreter(),
                    "native",
                    std::path::Path::new("/trusted/native.so"),
                    &"b".repeat(64)
                )
                .unwrap()
                .unsupported,
            &["signature unavailable for native.answer"]
        );
    }
}

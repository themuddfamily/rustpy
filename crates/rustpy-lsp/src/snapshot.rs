//! What a request handler sees: an immutable view of the server state taken
//! when the request arrived (documents, their analysis files, configuration).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use lsp_types::{Location, Position, Uri};
use rustpy_core::{CancellationToken, TextRange, TextSize};
use rustpy_semantic::{Analysis, FileInfo};

use crate::caps::ClientCaps;
use crate::config::Config;
use crate::diagnostics::FileProblems;
use crate::docs::Doc;
use crate::line_index::{Encoding, LineIndex};
use crate::paths::{self, StubDir};
use crate::project::ProjectFiles;
use crate::symbols::SymbolIndex;

/// An error answered to the client.
#[derive(Debug, Clone)]
pub struct LspError {
    pub code: i32,
    pub message: String,
}

impl LspError {
    pub fn new(code: lsp_server::ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code: code as i32,
            message: message.into(),
        }
    }
    pub fn cancelled() -> Self {
        Self::new(lsp_server::ErrorCode::RequestCanceled, "request cancelled")
    }
    pub fn content_modified() -> Self {
        Self::new(
            lsp_server::ErrorCode::ContentModified,
            "the document changed",
        )
    }
    /// The request was valid but failed (an explanation for the user).
    pub fn failed(message: impl Into<String>) -> Self {
        Self::new(lsp_server::ErrorCode::RequestFailed, message)
    }
    pub fn invalid_params(message: impl Into<String>) -> Self {
        Self::new(lsp_server::ErrorCode::InvalidParams, message)
    }
}

pub type HResult<T> = Result<T, LspError>;

/// A document a request refers to: an open document, or a file read from disk.
#[derive(Clone)]
pub struct DocView {
    pub uri: Uri,
    pub path: PathBuf,
    /// `None` for files that are not open.
    pub version: Option<i32>,
    pub index: Arc<LineIndex>,
    pub file: Arc<FileInfo>,
}

impl DocView {
    pub fn offset(&self, pos: Position, enc: Encoding) -> TextSize {
        self.index.offset(pos, enc)
    }
    pub fn text(&self) -> &str {
        self.index.text()
    }
}

pub type Generation = OnceLock<(Arc<Analysis>, Arc<SymbolIndex>)>;

#[derive(Clone)]
pub struct Snapshot {
    pub prepared: Arc<Generation>,
    pub analysis: Arc<Analysis>,
    pub config: Arc<Config>,
    pub caps: Arc<ClientCaps>,
    pub enc: Encoding,
    pub docs: Arc<HashMap<PathBuf, Doc>>,
    pub files: Arc<ProjectFiles>,
    pub index: Arc<SymbolIndex>,
    pub stubs: Arc<StubDir>,
    /// Inspection results of open documents, for code actions: `(version, problems)`.
    pub problems: Arc<HashMap<PathBuf, (i32, Arc<FileProblems>)>>,
    pub python_version: (u32, u32),
    /// The probed project interpreter (external formatters run with it).
    pub interpreter: Option<PathBuf>,
    /// Whether the IDE trusts the project (only then may project tools run).
    pub trusted: bool,
    pub cancel: CancellationToken,
}

/// The analysis file of `path`: virtual stubs are looked up by module name.
pub fn analysis_file(analysis: &Analysis, path: &Path) -> Option<Arc<FileInfo>> {
    if paths::is_virtual_stub(path) {
        if let Some(f) = analysis.file_for_path(path) {
            return Some(f);
        }
        return analysis.module(&paths::stub_module_name(path)?);
    }
    analysis.file_for_path(path)
}

impl Snapshot {
    pub fn project_files(&self) -> Arc<Vec<PathBuf>> {
        let files = self.files.get();
        let extra: Vec<_> = self
            .docs
            .values()
            .filter(|d| {
                d.is_python && self.files.contains(&d.path) && files.binary_search(&d.path).is_err()
            })
            .map(|d| d.path.clone())
            .collect();
        if extra.is_empty() {
            return files;
        }
        let mut files = (*files).clone();
        files.extend(extra);
        files.sort();
        Arc::new(files)
    }
    /// Prepare a document generation once, on a worker. Requests may share
    /// analysis caches, but later notifications never modify this database.
    pub fn prepare(&mut self) {
        let (analysis, index) = self.prepared.get_or_init(|| {
            let r = &self.analysis.resolver;
            let analysis = Analysis::new(
                rustpy_semantic::module::ModuleResolver::new(
                    r.roots.clone(),
                    r.search_paths.clone(),
                    r.python_version,
                )
                .with_platform(&r.platform)
                .with_stub_roots(r.stub_roots.iter().cloned()),
            );
            analysis.set_analyze_doc_comments(
                self.config
                    .settings
                    .documentation
                    .effective(
                        r.roots.first().map(PathBuf::as_path),
                        &self.config.settings.docstring_format,
                    )
                    .analyze_doc_comments,
            );
            analysis.set_cache_limits(self.analysis.cache_limits());
            analysis.set_runtime_types((*self.analysis.runtime_types()).clone());
            analysis.set_runtime_docs(self.analysis.runtime_docs());
            for doc in self
                .docs
                .values()
                .filter(|d| d.is_python && !paths::is_virtual_stub(&d.path))
            {
                analysis.set_file_text(&doc.path, doc.text());
            }
            let index = self.index.with_open(&self.docs, &self.files, &analysis);
            (Arc::new(analysis), Arc::new(index))
        });
        self.analysis = analysis.clone();
        self.index = index.clone();
    }
    pub fn check_cancelled(&self) -> HResult<()> {
        if self.cancel.is_cancelled() {
            Err(LspError::cancelled())
        } else {
            Ok(())
        }
    }

    /// The analysis path of a URI: real files as they are, stub copies as
    /// their virtual `<typeshed>` path.
    pub fn path_of(&self, uri: &Uri) -> Option<PathBuf> {
        let path = paths::path_from_uri(uri)?;
        Some(self.stubs.virtual_path(&path).unwrap_or(path))
    }

    /// The analysis file of `path`: virtual stubs are looked up by module name.
    pub fn file_for_path(&self, path: &Path) -> Option<Arc<FileInfo>> {
        analysis_file(&self.analysis, path)
    }

    /// The Python document a request is about.
    pub fn doc(&self, uri: &Uri) -> HResult<DocView> {
        let path = self
            .path_of(uri)
            .ok_or_else(|| LspError::invalid_params(format!("not a file URI: {}", uri.as_str())))?;
        if let Some(doc) = self.docs.get(&path) {
            if !doc.is_python {
                return Err(LspError::failed("not a Python file"));
            }
            let file = self
                .file_for_path(&path)
                .filter(|f| *f.text == *doc.text())
                .ok_or_else(LspError::content_modified)?;
            return Ok(DocView {
                uri: doc.uri.clone(),
                path,
                version: Some(doc.version),
                index: doc.index.clone(),
                file,
            });
        }
        if !matches!(
            path.extension().and_then(|e| e.to_str()),
            Some("py" | "pyi" | "pyw")
        ) {
            return Err(LspError::failed("not a Python file"));
        }
        let file = self.file_for_path(&path).ok_or_else(|| {
            LspError::failed(
                self.analysis
                    .file_encoding_error(&path)
                    .map(|e| format!("Cannot decode {}: {e}", path.display()))
                    .unwrap_or_else(|| format!("cannot read {}", path.display())),
            )
        })?;
        Ok(DocView {
            uri: uri.clone(),
            path,
            version: None,
            index: Arc::new(LineIndex::new(file.text.clone())),
            file,
        })
    }

    /// The URI to report for `path`: the open document's own URI, a
    /// materialized stub, or a `file:` URI.
    pub fn uri_for(&self, path: &Path) -> Option<Uri> {
        if let Some(d) = self.docs.get(path) {
            return Some(d.uri.clone());
        }
        if paths::is_virtual_stub(path) {
            let file = self.file_for_path(path)?;
            let real = self.stubs.materialize(path, &file.text)?;
            return paths::uri_from_path(&real);
        }
        paths::uri_from_path(path)
    }

    /// Line starts of `path` as the analysis sees it (open text or disk copy).
    pub fn line_index(&self, path: &Path) -> Option<Arc<LineIndex>> {
        if let Some(d) = self.docs.get(path) {
            return Some(d.index.clone());
        }
        let file = self.file_for_path(path)?;
        Some(Arc::new(LineIndex::new(file.text.clone())))
    }

    /// Converts many `(path, range)` pairs, reading each file's lines once.
    pub fn locations(
        &self,
        items: impl IntoIterator<Item = (PathBuf, TextRange)>,
    ) -> Vec<Location> {
        let mut cache: HashMap<PathBuf, Option<(Uri, Arc<LineIndex>)>> = HashMap::new();
        let mut out = Vec::new();
        for (path, range) in items {
            let entry = cache
                .entry(path.clone())
                .or_insert_with(|| Some((self.uri_for(&path)?, self.line_index(&path)?)));
            if let Some((uri, index)) = entry {
                out.push(Location::new(uri.clone(), index.range(range, self.enc)));
            }
        }
        out
    }

    /// The document version the client must have for edits of `path`
    /// (`None` for files that are not open).
    pub fn version_of(&self, path: &Path) -> Option<i32> {
        self.docs.get(path).map(|d| d.version)
    }
}

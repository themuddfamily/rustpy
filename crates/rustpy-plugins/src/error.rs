//! The crate's error type.

use std::path::{Path, PathBuf};

/// Everything that can go wrong while reading, installing or downloading plugins.
///
/// The `Display` text is written for the user (Settings › Plugins shows it as is).
#[derive(Debug, thiserror::Error)]
pub enum PluginError {
    /// Reading or writing a file or directory failed.
    #[error("{}: {source}", path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// A JSON document (manifest, catalog, …) is not well-formed or has wrongly typed fields.
    #[error("{what}: {source}")]
    Json {
        what: String,
        #[source]
        source: serde_json::Error,
    },
    /// The plugin failed validation; every problem found is listed.
    #[error("invalid plugin: {}", problems.join("; "))]
    Invalid { problems: Vec<String> },
    /// No installed plugin has this id.
    #[error("plugin `{0}` is not installed")]
    NotInstalled(String),
    /// The plugin is installed already and the offered version is not newer.
    #[error("plugin `{id}` {installed} is already installed; only a newer version can replace it (offered: {offered})")]
    AlreadyInstalled {
        id: String,
        installed: String,
        offered: String,
    },
    /// The plugin's `min_ide_version` is newer than this IDE.
    #[error("plugin `{id}` requires RustPy {required} or newer (this is {current})")]
    Incompatible {
        id: String,
        required: String,
        current: String,
    },
    /// An HTTP request failed (transport error, HTTP error status, oversized body …).
    #[error("download of {url} failed: {message}")]
    Download { url: String, message: String },
    /// A download's SHA-256 does not match the catalog.
    #[error("checksum mismatch for {url}: expected sha256 {expected}, got {actual}")]
    ChecksumMismatch {
        url: String,
        expected: String,
        actual: String,
    },
    /// A plugin bundle (`.rpplugin`) is malformed or unsafe.
    #[error("invalid plugin bundle: {0}")]
    Bundle(String),
    /// A catalog (or one of its entries) cannot be used.
    #[error("invalid plugin catalog: {0}")]
    Catalog(String),
}

impl PluginError {
    /// The individual validation problems ([`PluginError::Invalid`]), or the message itself.
    pub fn problems(&self) -> Vec<String> {
        match self {
            PluginError::Invalid { problems } => problems.clone(),
            other => vec![other.to_string()],
        }
    }

    pub(crate) fn invalid(problem: impl Into<String>) -> Self {
        PluginError::Invalid {
            problems: vec![problem.into()],
        }
    }
}

/// `map_err` adapter attaching the path to an I/O error.
pub(crate) fn io_err(path: &Path) -> impl FnOnce(std::io::Error) -> PluginError + '_ {
    move |source| PluginError::Io {
        path: path.to_path_buf(),
        source,
    }
}

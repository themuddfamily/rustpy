//! Downloaded bundles are checked and staged by the worker; the UI publishes only
//! its still-owned successful result. Cancellation drops the staged replacement.
#[cfg(test)]
mod configured_https_tests;
use rustpy_core::{ProgressReporter, TaskError};
use rustpy_index::{IndexKind, IndexStore, StagedIndex};
use sha2::{Digest, Sha256};
use std::{
    io::Read,
    path::PathBuf,
    time::{Duration, Instant},
};
#[derive(Clone, Debug)]
pub(crate) struct Request {
    pub url: String,
    pub sha256: String,
    pub limit: u64,
}
pub(crate) struct Ready {
    pub stage: StagedIndex,
    pub path: PathBuf,
    pub label: String,
    pub files: usize,
}
fn address(value: &str) -> Result<url::Url, String> {
    let url = url::Url::parse(value).map_err(|error| error.to_string())?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err("Use an HTTPS bundle URL without embedded credentials or fragments".into());
    }
    Ok(url)
}
impl Request {
    pub fn validate(&self) -> Result<(), String> {
        address(&self.url)?;
        if self.sha256.len() != 64 || !self.sha256.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err("Enter the publisher's 64-digit SHA-256 for this bundle".into());
        }
        if self.limit == 0 || self.limit > rustpy_index::format::MAX_FILE {
            return Err("Invalid bundle size limit".into());
        }
        Ok(())
    }
    pub fn host(&self) -> String {
        address(&self.url)
            .ok()
            .and_then(|url| url.host_str().map(str::to_owned))
            .unwrap_or_default()
    }
}
fn failed(message: impl std::fmt::Display) -> TaskError {
    TaskError::Failed(message.to_string())
}
fn verified_bundle(
    bytes: &[u8],
    expected: &str,
    limit: u64,
) -> Result<rustpy_index::SymbolIndex, String> {
    if bytes.len() as u64 > limit {
        return Err("Downloaded index exceeds the configured size limit".into());
    }
    let actual = format!("{:x}", Sha256::digest(bytes));
    if !actual.eq_ignore_ascii_case(expected) {
        return Err("Downloaded index SHA-256 does not match; no bundle was installed".into());
    }
    let index = rustpy_index::format::decode(bytes).map_err(|error| error.to_string())?;
    if index.meta().kind != IndexKind::Bundle || index.file_count() == 0 {
        return Err("The download must be a nonempty shared .rpidx bundle".into());
    }
    Ok(index)
}
pub(crate) fn fetch(
    request: Request,
    store: IndexStore,
    progress: &ProgressReporter,
) -> Result<Ready, TaskError> {
    request.validate().map_err(failed)?;
    let mut url = address(&request.url).map_err(failed)?;
    let started = Instant::now();
    for redirect in 0..=5 {
        progress.check()?;
        let response = rustpy_net::agent_builder(url.as_str())
            .redirects(0)
            .timeout_connect(Duration::from_secs(10))
            .timeout_read(Duration::from_secs(10))
            .timeout(Duration::from_secs(90))
            .build()
            .get(url.as_str())
            .set("User-Agent", "RustPy shared indexes")
            .set("Accept", "application/octet-stream")
            .call()
            .map_err(failed)?;
        if matches!(response.status(), 301 | 302 | 303 | 307 | 308) {
            if redirect == 5 {
                return Err(failed("Too many shared-index redirects"));
            }
            let target = response
                .header("Location")
                .ok_or_else(|| failed("Bundle redirect has no Location"))?;
            url = address(url.join(target).map_err(failed)?.as_str()).map_err(failed)?;
            continue;
        }
        if response.status() != 200 {
            return Err(failed(format!(
                "Bundle server returned HTTP {}",
                response.status()
            )));
        }
        let length = response
            .header("Content-Length")
            .and_then(|value| value.parse::<u64>().ok());
        if length.is_some_and(|length| length > request.limit) {
            return Err(failed("Bundle exceeds the configured download limit"));
        }
        let mut reader = response.into_reader();
        let mut bytes = Vec::new();
        let mut chunk = [0u8; 16 * 1024];
        loop {
            progress.check()?;
            if started.elapsed() > Duration::from_secs(90) {
                return Err(failed("Shared-index download exceeded 90 seconds"));
            }
            let count = reader.read(&mut chunk).map_err(failed)?;
            if count == 0 {
                break;
            }
            if (bytes.len() + count) as u64 > request.limit {
                return Err(failed("Bundle exceeds the configured download limit"));
            }
            bytes.extend_from_slice(&chunk[..count]);
            progress.set_details(format!(
                "{} bytes received from {}",
                bytes.len(),
                request.host()
            ));
            if let Some(length) = length.filter(|length| *length > 0) {
                progress.set_fraction((bytes.len() as f64 / length as f64).min(0.9));
            }
        }
        progress.check()?;
        let index = verified_bundle(&bytes, &request.sha256, request.limit).map_err(failed)?;
        progress.check()?;
        let path = store
            .shared_dir()
            .join(format!("{}.rpidx", request.sha256.to_ascii_lowercase()));
        let label = index.meta().label.clone();
        let files = index.file_count();
        let stage = IndexStore::stage(&path, &index).map_err(failed)?;
        progress.check()?;
        return Ok(Ready {
            stage,
            path,
            label,
            files,
        });
    }
    Err(failed("Shared-index download did not produce a response"))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn validation_rejects_credentials_unsafe_redirect_targets_and_bad_checksums() {
        for url in [
            "http://example.com/index.rpidx",
            "https://user:password@example.com/a",
            "file:///tmp/a",
            "https://example.com/a#fragment",
        ] {
            assert!(Request {
                url: url.into(),
                sha256: "a".repeat(64),
                limit: 1024
            }
            .validate()
            .is_err());
        }
        assert!(Request {
            url: "https://example.com/stdlib.rpidx".into(),
            sha256: "g".repeat(64),
            limit: 1024
        }
        .validate()
        .is_err());
        assert!(verified_bundle(b"corrupt", &"0".repeat(64), 1024)
            .err()
            .unwrap()
            .contains("SHA-256"));
    }
}

#[cfg(test)]
mod bundle_fixtures {
    use super::*;
    use rustpy_index::{refresh, BuildOptions, IndexMeta, RootKind, RootSpec, SourceFile};
    use std::sync::Arc;
    fn bundle(root: &std::path::Path) -> Vec<u8> {
        std::fs::create_dir_all(root).unwrap();
        let path = root.join("library.py");
        std::fs::write(&path, "def exported(): pass\n").unwrap();
        let (index, _) = refresh(
            None,
            IndexMeta::default(),
            &[RootSpec {
                path: root.into(),
                kind: RootKind::Stdlib,
            }],
            vec![SourceFile { root: 0, path }],
            &[],
            &BuildOptions {
                threads: 1,
                ..Default::default()
            },
            &Default::default(),
        )
        .unwrap();
        let bundle = index.unwrap().to_bundle(
            &[RootKind::Stdlib],
            IndexMeta {
                label: "stdlib".into(),
                python_version: "3.13".into(),
                ..Default::default()
            },
        );
        rustpy_index::format::encode(&bundle)
    }
    #[test]
    fn verified_download_reuses_only_identical_local_library_content() {
        let dir = tempfile::tempdir().unwrap();
        let bytes = bundle(&dir.path().join("publisher"));
        let hash = format!("{:x}", Sha256::digest(&bytes));
        let verified = Arc::new(verified_bundle(&bytes, &hash, 1024 * 1024).unwrap());
        let root = dir.path().join("recipient");
        std::fs::create_dir(&root).unwrap();
        let file = root.join("library.py");
        std::fs::write(&file, "def exported(): pass\n").unwrap();
        let roots = [RootSpec {
            path: root,
            kind: RootKind::Stdlib,
        }];
        let files = vec![SourceFile {
            root: 0,
            path: file.clone(),
        }];
        let options = BuildOptions {
            threads: 1,
            ..Default::default()
        };
        let (_, stats) = refresh(
            None,
            IndexMeta::default(),
            &roots,
            files.clone(),
            std::slice::from_ref(&verified),
            &options,
            &Default::default(),
        )
        .unwrap();
        assert_eq!(stats.from_shared, 1);
        assert_eq!(stats.parsed, 0);
        std::fs::write(&file, "def changed(): pass\n").unwrap();
        let (_, stats) = refresh(
            None,
            IndexMeta::default(),
            &roots,
            files,
            &[verified],
            &options,
            &Default::default(),
        )
        .unwrap();
        assert_eq!(stats.from_shared, 0);
        assert_eq!(stats.parsed, 1);
    }
    #[test]
    fn unaccepted_staged_download_keeps_previous_cache_and_future_formats_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let mut bytes = bundle(&dir.path().join("publisher"));
        let hash = format!("{:x}", Sha256::digest(&bytes));
        let index = verified_bundle(&bytes, &hash, 1024 * 1024).unwrap();
        let destination = dir.path().join("shared.rpidx");
        std::fs::write(&destination, b"previous").unwrap();
        let staged = IndexStore::stage(&destination, &index).unwrap();
        let ready = Ready {
            stage: staged,
            path: destination.clone(),
            label: "test".into(),
            files: 1,
        };
        drop(ready);
        assert_eq!(std::fs::read(&destination).unwrap(), b"previous");
        bytes[8..12].copy_from_slice(&u32::MAX.to_le_bytes());
        let hash = format!("{:x}", Sha256::digest(&bytes));
        assert!(verified_bundle(&bytes, &hash, 1024 * 1024).is_err());
    }
}

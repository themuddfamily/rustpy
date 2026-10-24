//! Apply the captured server policy to every transport call, including legacy callers.
use crate::{options::Options, transfer, RemoteEntry, Transport};
use std::path::Path;
pub(crate) struct PolicyTransport {
    pub inner: Box<dyn Transport>,
    pub options: Options,
}
impl PolicyTransport {
    fn call<T>(&self, label: &str, f: impl FnOnce() -> anyhow::Result<T>) -> anyhow::Result<T> {
        let mut context = transfer::current();
        context.options = self.options.clone();
        transfer::with_context(context, || {
            transfer::check()?;
            transfer::note(label);
            f()
        })
    }
}
impl Transport for PolicyTransport {
    fn upload(&self, local: &Path, remote: &str) -> anyhow::Result<()> {
        self.call(&format!("Upload {remote}"), || {
            self.inner.upload(local, remote)
        })
    }
    fn download(&self, remote: &str, local: &Path) -> anyhow::Result<()> {
        self.call(&format!("Download {remote}"), || {
            self.inner.download(remote, local)
        })
    }
    fn list(&self, remote: &str) -> anyhow::Result<Vec<RemoteEntry>> {
        self.call(&format!("List {remote}"), || self.inner.list(remote))
    }
    fn delete(&self, remote: &str) -> anyhow::Result<()> {
        self.call(&format!("Delete {remote}"), || self.inner.delete(remote))
    }
    fn mkdirs(&self, remote: &str) -> anyhow::Result<()> {
        self.call(&format!("Create directory {remote}"), || {
            self.inner.mkdirs(remote)
        })
    }
    fn rename(&self, source: &str, destination: &str) -> anyhow::Result<()> {
        self.call(&format!("Rename {source}"), || {
            self.inner.rename(source, destination)
        })
    }
    fn permissions(&self, path: &str, mode: u32) -> anyhow::Result<()> {
        self.call(&format!("Permissions {path}"), || {
            self.inner.permissions(path, mode)
        })
    }
    fn test_connection(&self) -> anyhow::Result<String> {
        self.call("Testing connection", || self.inner.test_connection())
    }
    fn coarse_times(&self) -> bool {
        self.inner.coarse_times()
    }
    fn creates_parent_dirs(&self) -> bool {
        self.inner.creates_parent_dirs()
    }
    fn describe(&self) -> String {
        self.inner.describe()
    }
    fn list_recursive(&self, root: &str, excluded: &[String]) -> anyhow::Result<Vec<RemoteEntry>> {
        let mut patterns = excluded.to_vec();
        patterns.extend(self.options.excluded.clone());
        self.call(&format!("List tree {root}"), || {
            self.inner.list_recursive(root, &patterns)
        })
    }
}

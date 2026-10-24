//! Logical shell ownership. Endpoint publication is retryable after the exact
//! durable token is retained; neither discovery nor this adapter approves Quit.
use rustpy_cli::{
    ipc::{QuitStateStamp, Server},
    window_admission::AdmissionLease,
    window_registry,
};
use rustpy_settings::window_session_store::{WindowOwnerId, WindowSessionStore, WindowToken};
use std::{
    io,
    path::{Path, PathBuf},
};

#[derive(Debug)]
pub struct WindowOwnerLifetime {
    config: PathBuf,
    store: WindowSessionStore,
    token: WindowToken,
    project: Option<PathBuf>,
    endpoint: Option<(String, u16)>,
    retired: bool,
}
fn error(message: impl std::fmt::Display) -> io::Error {
    io::Error::other(message.to_string())
}
fn project_path(config: &Path, project: Option<&Path>) -> Option<PathBuf> {
    project.map(|path| {
        rustpy_core::vfs::normalize_path(&if path.is_absolute() {
            path.into()
        } else {
            config.join(path)
        })
    })
}
impl WindowOwnerLifetime {
    /// Durable acceptance only. Retain the returned owner BEFORE calling sync;
    /// a publication failure must never cause a second accept.
    pub fn accept(
        config: &Path,
        server: &Server,
        project: Option<&Path>,
        lease: &AdmissionLease,
    ) -> io::Result<Self> {
        let config = dunce::canonicalize(config)?;
        Self::validate_lease(&config, lease)?;
        Self::validate_endpoint(&config, server)?;
        server.set_quit_state(None)?;
        Self::validate_lease(&config, lease)?;
        let endpoint = (server.window_id(), server.port());
        let store = WindowSessionStore::new(&config);
        let baseline = store.load().map_err(error)?;
        Self::validate_lease(&config, lease)?;
        let mutation = store
            .accept_window(&baseline, WindowOwnerId::new(), project)
            .map_err(error)?;
        // Nothing fallible after acceptance: the caller always receives its token.
        let project = mutation
            .snapshot
            .live_windows()
            .into_iter()
            .find(|record| record.token == mutation.token)
            .expect("accepted owner exists")
            .project;
        Ok(Self {
            config,
            store,
            token: mutation.token,
            project,
            endpoint: Some(endpoint),
            retired: false,
        })
    }
    pub fn from_accepted(
        config: &Path,
        token: WindowToken,
        project: Option<&Path>,
    ) -> io::Result<Self> {
        let config = dunce::canonicalize(config)?;
        let store = WindowSessionStore::new(&config);
        let actual = store
            .load()
            .map_err(error)?
            .live_windows()
            .into_iter()
            .find(|record| record.token == token)
            .ok_or_else(|| {
                error("accepted window token is stale or belongs to another configuration")
            })?;
        if actual.project != project_path(&config, project) {
            return Err(error("accepted window project differs"));
        }
        Ok(Self {
            config,
            store,
            token,
            project: actual.project,
            endpoint: None,
            retired: false,
        })
    }
    pub fn token(&self) -> &WindowToken {
        &self.token
    }
    #[cfg(test)]
    pub fn store(&self) -> &WindowSessionStore {
        &self.store
    }
    #[cfg(test)]
    pub fn project(&self) -> Option<&Path> {
        self.project.as_deref()
    }
    pub fn state_stamp(&self) -> QuitStateStamp {
        QuitStateStamp {
            owner: self.token.owner().as_str().into(),
            generation: self.token.generation().value(),
            owner_revision: self.token.revision(),
        }
    }
    fn validate_lease(config: &Path, lease: &AdmissionLease) -> io::Result<()> {
        lease.validate_identity().map_err(error)?;
        if lease.configuration() != config {
            return Err(error("admission belongs to another configuration"));
        }
        Ok(())
    }
    fn validate_endpoint(config: &Path, server: &Server) -> io::Result<()> {
        if !window_registry::live_windows(config)?
            .iter()
            .any(|window| window.id == server.window_id() && window.endpoint.port == server.port())
        {
            return Err(error("server belongs to another configuration"));
        }
        Ok(())
    }
    fn validate(&mut self, server: &Server, lease: &AdmissionLease) -> io::Result<()> {
        Self::validate_lease(&self.config, lease)?;
        if let Some((id, port)) = &self.endpoint {
            if *id != server.window_id() || *port != server.port() {
                return Err(error("owner belongs to another endpoint"));
            }
        } else {
            Self::validate_endpoint(&self.config, server)?;
            self.endpoint = Some((server.window_id(), server.port()));
        }
        Ok(())
    }
    fn validate_or_invalidate(
        &mut self,
        server: &Server,
        lease: &AdmissionLease,
    ) -> io::Result<()> {
        if let Err(validation) = self.validate(server, lease) {
            // Revocation of our own published cache is required even when the
            // admission capability was revoked. Never touch a foreign endpoint
            // or an unbound restored owner that has published nothing yet.
            if self
                .endpoint
                .as_ref()
                .is_some_and(|(id, port)| *id == server.window_id() && *port == server.port())
            {
                if let Err(invalidation) = server.set_quit_state(None) {
                    return Err(error(format!(
                        "{validation}; owner discovery invalidation failed: {invalidation}"
                    )));
                }
            }
            return Err(validation);
        }
        Ok(())
    }
    /// Validate even a no-op against the exact token. Never rebaseline a stale
    /// owner. Every failure clears discovery, including A -> failed B -> A.
    pub fn sync(
        &mut self,
        server: &Server,
        actual_project: Option<&Path>,
        lease: &AdmissionLease,
    ) -> io::Result<()> {
        self.validate_or_invalidate(server, lease)?;
        server.set_quit_state(None)?;
        if self.retired {
            return Err(error("window owner is retired"));
        }
        let record = self
            .store
            .load()
            .map_err(error)?
            .live_windows()
            .into_iter()
            .find(|record| record.token == self.token)
            .ok_or_else(|| error("window owner token is stale"))?;
        if record.project != self.project {
            return Err(error("window owner project changed"));
        }
        let project = project_path(&self.config, actual_project);
        if project != self.project {
            Self::validate_lease(&self.config, lease)?;
            let mutation = self
                .store
                .update_project(&self.token, actual_project)
                .map_err(error)?;
            self.token = mutation.token;
            self.project = project;
        }
        Self::validate_lease(&self.config, lease)?;
        server.set_project(self.project.as_deref())?;
        Self::validate_lease(&self.config, lease)?;
        server.set_quit_state(Some(self.state_stamp()))
    }
    /// Ordinary close only. After a globally committed Quit the caller skips
    /// this method: an unknown/stale durable token is never treated as success.
    pub fn retire(&mut self, server: &Server, lease: &AdmissionLease) -> io::Result<()> {
        self.validate_or_invalidate(server, lease)?;
        server.set_quit_state(None)?;
        if self.retired {
            return Ok(());
        }
        Self::validate_lease(&self.config, lease)?;
        self.store.close_window(&self.token).map_err(error)?;
        self.retired = true;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustpy_cli::{ipc::query_window_owner, window_admission::WindowAdmission};
    use std::time::{Duration, Instant};
    fn lease(config: &Path) -> AdmissionLease {
        WindowAdmission::new(config)
            .unwrap()
            .acquire_shared(Instant::now() + Duration::from_secs(2))
            .unwrap()
    }
    fn probe(config: &Path, server: &Server) -> io::Result<QuitStateStamp> {
        let window = window_registry::live_windows(config)?
            .into_iter()
            .find(|window| window.id == server.window_id())
            .unwrap();
        query_window_owner(&window, Duration::from_secs(2))
    }
    // Authored for final verification; never executed during feature work.
    #[test]
    fn native_router_keeps_same_process_project_owners_independent() {
        let config = tempfile::tempdir().unwrap();
        let admission = lease(config.path());
        let rustpy_cli::ipc::Probe::Free(instance) = rustpy_cli::ipc::probe(config.path()).unwrap()
        else {
            panic!("fixture instance must be unowned");
        };
        let router = Server::start(instance).unwrap();
        router.become_native_project_router().unwrap();
        let first = Server::start_secondary(config.path()).unwrap();
        let second = Server::start_secondary(config.path()).unwrap();
        let a = config.path().join("project-a");
        let b = config.path().join("project-b");
        std::fs::create_dir(&a).unwrap();
        std::fs::create_dir(&b).unwrap();
        let mut owner_a =
            WindowOwnerLifetime::accept(config.path(), &first, Some(&a), &admission).unwrap();
        let mut owner_b =
            WindowOwnerLifetime::accept(config.path(), &second, Some(&b), &admission).unwrap();
        owner_a.sync(&first, Some(&a), &admission).unwrap();
        owner_b.sync(&second, Some(&b), &admission).unwrap();
        assert_ne!(first.window_id(), second.window_id());
        assert_ne!(owner_a.token().owner(), owner_b.token().owner());
        assert_eq!(owner_a.store().load().unwrap().live_windows().len(), 2);
        let survivor = probe(config.path(), &second).unwrap();
        owner_a.retire(&first, &admission).unwrap();
        drop(first);
        assert_eq!(owner_b.store().load().unwrap().live_windows().len(), 1);
        assert_eq!(probe(config.path(), &second).unwrap(), survivor);
        owner_b.retire(&second, &admission).unwrap();
        drop(second);
        assert!(owner_b.store().load().unwrap().live_windows().is_empty());
        drop(router);
    }
    #[test]
    fn foreign_endpoint_validation_failure_does_not_clear_foreign_owner() {
        let config = tempfile::tempdir().unwrap();
        let admission = lease(config.path());
        let owned = Server::start_secondary(config.path()).unwrap();
        let foreign = Server::start_secondary(config.path()).unwrap();
        let mut first =
            WindowOwnerLifetime::accept(config.path(), &owned, None, &admission).unwrap();
        let mut second =
            WindowOwnerLifetime::accept(config.path(), &foreign, None, &admission).unwrap();
        first.sync(&owned, None, &admission).unwrap();
        second.sync(&foreign, None, &admission).unwrap();
        let foreign_stamp = probe(config.path(), &foreign).unwrap();
        assert!(first.sync(&foreign, None, &admission).is_err());
        assert_eq!(probe(config.path(), &foreign).unwrap(), foreign_stamp);
        assert!(first.retire(&foreign, &admission).is_err());
        assert_eq!(probe(config.path(), &foreign).unwrap(), foreign_stamp);
    }
    #[test]
    fn wrong_configuration_lease_invalidates_only_bound_endpoint_probe() {
        let config = tempfile::tempdir().unwrap();
        let wrong = tempfile::tempdir().unwrap();
        let admission = lease(config.path());
        let wrong_admission = lease(wrong.path());
        let server = Server::start_secondary(config.path()).unwrap();
        let mut lifetime =
            WindowOwnerLifetime::accept(config.path(), &server, None, &admission).unwrap();
        lifetime.sync(&server, None, &admission).unwrap();
        let token = lifetime.token().clone();
        assert!(lifetime
            .sync(&server, Some(config.path()), &wrong_admission)
            .is_err());
        assert_eq!(lifetime.token(), &token);
        assert_eq!(lifetime.project(), None);
        assert_eq!(
            lifetime.store().load().unwrap().live_windows()[0].token,
            token
        );
        assert!(
            probe(config.path(), &server).is_err(),
            "bound endpoint must not expose old ownership after rejected sync"
        );
    }
    #[cfg(unix)]
    #[test]
    fn replaced_admission_identity_invalidates_bound_endpoint_probe() {
        let config = tempfile::tempdir().unwrap();
        let admission = lease(config.path());
        let server = Server::start_secondary(config.path()).unwrap();
        let mut lifetime =
            WindowOwnerLifetime::accept(config.path(), &server, None, &admission).unwrap();
        lifetime.sync(&server, None, &admission).unwrap();
        let token = lifetime.token().clone();
        let lock = config.path().join("window-admission.lock");
        std::fs::rename(&lock, config.path().join("original-admission.lock")).unwrap();
        std::fs::write(&lock, b"").unwrap();
        assert!(lifetime
            .sync(&server, Some(config.path()), &admission)
            .is_err());
        assert_eq!(lifetime.token(), &token);
        assert_eq!(lifetime.project(), None);
        assert_eq!(
            lifetime.store().load().unwrap().live_windows()[0].token,
            token
        );
        assert!(
            probe(config.path(), &server).is_err(),
            "replaced lease must invalidate known owned endpoint"
        );
    }
    #[test]
    fn welcomes_switch_and_ordinary_close_keep_distinct_exact_owners() {
        let config = tempfile::tempdir().unwrap();
        let admission = lease(config.path());
        let first = Server::start_secondary(config.path()).unwrap();
        let second = Server::start_secondary(config.path()).unwrap();
        let mut a = WindowOwnerLifetime::accept(config.path(), &first, None, &admission).unwrap();
        let mut b = WindowOwnerLifetime::accept(config.path(), &second, None, &admission).unwrap();
        a.sync(&first, None, &admission).unwrap();
        b.sync(&second, None, &admission).unwrap();
        assert_ne!(
            probe(config.path(), &first).unwrap().owner,
            probe(config.path(), &second).unwrap().owner
        );
        let original = a.token().clone();
        let project = config.path().join("project");
        std::fs::create_dir(&project).unwrap();
        a.sync(&first, Some(&project), &admission).unwrap();
        assert_eq!(a.token().owner(), original.owner());
        assert_eq!(a.token().revision(), original.revision() + 1);
        assert_eq!(probe(config.path(), &first).unwrap(), a.state_stamp());
        a.sync(&first, None, &admission).unwrap();
        a.retire(&first, &admission).unwrap();
        a.retire(&first, &admission).unwrap();
        assert!(probe(config.path(), &first).is_err());
        assert_eq!(b.store().load().unwrap().live_windows().len(), 1);
        assert_eq!(probe(config.path(), &second).unwrap(), b.state_stamp());
    }
    #[test]
    fn failed_publication_retains_owner_and_return_to_old_project_is_checked() {
        let config = tempfile::tempdir().unwrap();
        let admission = lease(config.path());
        let server = Server::start_secondary(config.path()).unwrap();
        let a = config.path().join("a");
        let b = config.path().join("b");
        std::fs::create_dir(&a).unwrap();
        std::fs::create_dir(&b).unwrap();
        let mut lifetime =
            WindowOwnerLifetime::accept(config.path(), &server, Some(&a), &admission).unwrap();
        lifetime.sync(&server, Some(&a), &admission).unwrap();
        let old = lifetime.token().clone();
        let record = config
            .path()
            .join("windows")
            .join(server.window_id())
            .join("window.json");
        std::fs::remove_file(&record).unwrap();
        std::fs::create_dir(&record).unwrap();
        assert!(lifetime.sync(&server, Some(&b), &admission).is_err());
        assert_eq!(lifetime.token().owner(), old.owner());
        assert_eq!(lifetime.token().revision(), old.revision() + 1);
        assert_eq!(lifetime.project(), Some(b.as_path()));
        std::fs::remove_dir(&record).unwrap();
        lifetime.sync(&server, Some(&a), &admission).unwrap();
        assert_eq!(lifetime.token().revision(), old.revision() + 2);
        assert_eq!(
            probe(config.path(), &server).unwrap(),
            lifetime.state_stamp()
        );
        assert_eq!(lifetime.store().load().unwrap().live_windows().len(), 1);
    }
    #[test]
    fn stale_token_cannot_rebaseline_or_retire_competing_revision() {
        let config = tempfile::tempdir().unwrap();
        let admission = lease(config.path());
        let server = Server::start_secondary(config.path()).unwrap();
        let mut lifetime =
            WindowOwnerLifetime::accept(config.path(), &server, None, &admission).unwrap();
        lifetime.sync(&server, None, &admission).unwrap();
        let competitor = lifetime
            .store()
            .update_project(lifetime.token(), None)
            .unwrap();
        assert!(lifetime.sync(&server, None, &admission).is_err());
        assert!(lifetime.retire(&server, &admission).is_err());
        assert_eq!(
            lifetime.store().load().unwrap().live_windows()[0].token,
            competitor.token
        );
        assert!(probe(config.path(), &server).is_err());
        assert!(
            WindowOwnerLifetime::from_accepted(config.path(), lifetime.token().clone(), None)
                .is_err()
        );
        let restored =
            WindowOwnerLifetime::from_accepted(config.path(), competitor.token, None).unwrap();
        assert_eq!(restored.project(), None);
    }
}

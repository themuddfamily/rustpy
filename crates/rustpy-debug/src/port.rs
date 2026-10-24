//! Same-process ownership of debugpy's CLI listen ports. The OS listener must
//! be handed off before spawning debugpy; the logical lease remains until the
//! owning session is dropped. An unrelated external process can still race
//! that handoff. This registry does not claim cross-process port ownership.

use std::collections::HashMap;
use std::net::TcpListener;
use std::sync::{Mutex, OnceLock};

fn leased_ports() -> &'static Mutex<HashMap<u16, usize>> {
    static PORTS: OnceLock<Mutex<HashMap<u16, usize>>> = OnceLock::new();
    PORTS.get_or_init(|| Mutex::new(HashMap::new()))
}

pub(crate) struct DebugPortLease {
    port: u16,
    listener: Option<TcpListener>,
}

impl DebugPortLease {
    /// Logically claims an endpoint already listening in a console process.
    pub(crate) fn claim(port: u16) -> anyhow::Result<Self> {
        let mut ports = leased_ports().lock().unwrap_or_else(|e| e.into_inner());
        anyhow::ensure!(
            !ports.contains_key(&port),
            "Debug adapter port {port} is already owned by a session"
        );
        ports.insert(port, 1);
        Ok(Self {
            port,
            listener: None,
        })
    }

    pub(crate) fn shared(&self) -> Self {
        let mut ports = leased_ports()
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        *ports.entry(self.port).or_default() += 1;
        Self {
            port: self.port,
            listener: None,
        }
    }
    pub(crate) fn acquire() -> anyhow::Result<Self> {
        Self::allocate_with(|| TcpListener::bind("127.0.0.1:0"))
    }

    fn allocate_with(
        mut bind: impl FnMut() -> std::io::Result<TcpListener>,
    ) -> anyhow::Result<Self> {
        let mut ports = leased_ports().lock().unwrap_or_else(|e| e.into_inner());
        // Keep rejected OS listeners bound during allocation so the OS cannot
        // repeatedly offer the same logically leased port. Bound the work even
        // if all candidates conflict; no lock spans spawning or connecting.
        let mut rejected = Vec::new();
        for _ in 0..128 {
            let listener = bind()?;
            let port = listener.local_addr()?.port();
            if let std::collections::hash_map::Entry::Vacant(entry) = ports.entry(port) {
                entry.insert(1);
                return Ok(Self {
                    port,
                    listener: Some(listener),
                });
            }
            rejected.push(listener);
        }
        anyhow::bail!("Cannot allocate an unleased debug adapter port")
    }

    #[cfg(test)]
    pub(crate) fn port(&self) -> u16 {
        self.port
    }

    pub(crate) fn hand_off(&mut self) {
        self.listener = None;
    }
}

impl Drop for DebugPortLease {
    fn drop(&mut self) {
        self.listener = None;
        let mut ports = leased_ports()
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if let Some(count) = ports.get_mut(&self.port) {
            *count -= 1;
            if *count == 0 {
                ports.remove(&self.port);
            }
        }
    }
}

#[cfg(test)]
pub(crate) fn is_leased(port: u16) -> bool {
    leased_ports().lock().unwrap().contains_key(&port)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allocation_rejects_an_os_reoffered_live_session_port() {
        let mut first = DebugPortLease::acquire().unwrap();
        let port = first.port();
        let reservation = first.listener.as_ref().unwrap().try_clone().unwrap();
        first.hand_off();
        // Supply a real conflicting candidate while retaining a fixture-owned
        // physical reservation. Only the production logical lease changes;
        // parallel OS users cannot take the port between these assertions.
        let mut attempts = 0;
        let second = DebugPortLease::allocate_with(|| {
            attempts += 1;
            if attempts == 1 {
                reservation.try_clone()
            } else {
                TcpListener::bind("127.0.0.1:0")
            }
        })
        .unwrap();
        assert_eq!(attempts, 2);
        assert_ne!(port, second.port());
        assert!(leased_ports().lock().unwrap().contains_key(&port));
        drop(first);
        assert!(!leased_ports().lock().unwrap().contains_key(&port));
        let reused = DebugPortLease::allocate_with(|| reservation.try_clone()).unwrap();
        assert_eq!(reused.port(), port);
    }

    #[test]
    fn reservation_is_held_until_handoff_and_errors_do_not_leak_it() {
        let lease = DebugPortLease::acquire().unwrap();
        let port = lease.port();
        let reservation = lease.listener.as_ref().unwrap().try_clone().unwrap();
        assert!(TcpListener::bind(("127.0.0.1", port)).is_err());
        let error =
            DebugPortLease::allocate_with(|| Err(std::io::Error::other("fixture bind error")));
        assert!(error.is_err());
        assert!(leased_ports().lock().unwrap().contains_key(&port));
        drop(lease);
        assert!(!leased_ports().lock().unwrap().contains_key(&port));
        let reused = DebugPortLease::allocate_with(|| reservation.try_clone()).unwrap();
        assert_eq!(reused.port(), port);
    }
    #[test]
    fn exhausted_candidates_fail_after_bounded_allocation_work() {
        let lease = DebugPortLease::acquire().unwrap();
        let mut attempts = 0;
        let result = DebugPortLease::allocate_with(|| {
            attempts += 1;
            // A real listener with a repeatedly conflicting local address,
            // without opening an external bind race in the test fixture.
            lease.listener.as_ref().unwrap().try_clone()
        });
        assert!(result.is_err());
        assert_eq!(attempts, 128);
        assert!(is_leased(lease.port()));
    }
}

#[cfg(test)]
mod console_endpoint_tests {
    use super::*;

    #[test]
    fn attached_endpoints_have_exclusive_logical_ownership() {
        let first = DebugPortLease::acquire().unwrap();
        let port = first.port();
        assert!(DebugPortLease::claim(port).is_err());
        drop(first);
        let attached = DebugPortLease::claim(port).unwrap();
        assert!(is_leased(port));
        assert!(DebugPortLease::claim(port).is_err());
        drop(attached);
        assert!(!is_leased(port));
    }
}

#[cfg(test)]
mod shared_endpoint_tests {
    use super::*;
    #[test]
    fn subprocess_lease_outlives_parent_without_allowing_unrelated_claim() {
        let parent = DebugPortLease::acquire().unwrap();
        let port = parent.port();
        let child = parent.shared();
        drop(parent);
        assert!(DebugPortLease::claim(port).is_err());
        drop(child);
        assert!(DebugPortLease::claim(port).is_ok());
    }
}

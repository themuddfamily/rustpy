//! Server certificate verification that can ask the user.
//!
//! [`ConfirmingVerifier`] runs the normal WebPKI checks (bundled Mozilla roots,
//! the system CA bundle, imported CA certificates). When they fail it accepts
//! the certificate if exactly this certificate was accepted for this host;
//! with "accept non-trusted certificates automatically" it accepts and
//! remembers it for the host; otherwise it queues a [`CertificatePrompt`] and
//! the connection waits (up to [`PROMPT_TIMEOUT`]) for the user's answer, given
//! through [`answer_prompt`]. Connections made on the UI thread never wait:
//! they fail, the prompt stays visible, and accepting it makes the next attempt
//! succeed. A rejected certificate is not asked about again for five minutes,
//! so a batch of requests does not repeat the question. An accepted certificate
//! is pinned to its host only (see [`crate::certs`]).

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock, RwLock};
use std::thread::ThreadId;
use std::time::{Duration, Instant};

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::client::WebPkiServerVerifier;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{CertificateError, DigitallySignedStruct, Error, SignatureScheme};
use ureq::rustls;

use crate::certs::{is_accepted_for, CertStore};
use crate::x509::{format_time, sha256, CertificateInfo};

/// How long a connection waits for the answer to a certificate prompt.
pub const PROMPT_TIMEOUT: Duration = Duration::from_secs(120);
/// A rejected certificate is not asked about again for this long.
pub const REJECTION_MEMORY: Duration = Duration::from_secs(300);

/// A certificate waiting for the user's decision.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CertificatePrompt {
    pub id: u64,
    /// Host name or IP address the connection was for.
    pub host: String,
    /// Why the certificate is not trusted.
    pub problem: String,
    /// The server certificate first, then the other certificates the server sent.
    pub chain: Vec<CertificateInfo>,
    /// A connection is waiting for the answer. Otherwise accepting only helps the
    /// next attempt.
    pub waiting: bool,
}

struct Pending {
    prompt: CertificatePrompt,
    fingerprint: [u8; 32],
    der: Vec<u8>,
    store: CertStore,
    waiters: usize,
}

#[derive(Default)]
struct Prompts {
    pending: Vec<Pending>,
    /// Recent answers by prompt id, read by the connections that waited.
    answered: Vec<(u64, bool, Instant)>,
    /// Recently rejected certificates, per host.
    rejected: HashMap<([u8; 32], String), Instant>,
    next_id: u64,
}

fn prompts() -> MutexGuard<'static, Prompts> {
    static PROMPTS: OnceLock<Mutex<Prompts>> = OnceLock::new();
    PROMPTS
        .get_or_init(|| {
            Mutex::new(Prompts {
                next_id: 1,
                ..Default::default()
            })
        })
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

static ANSWERED: Condvar = Condvar::new();

type Notify = Arc<dyn Fn() + Send + Sync>;

struct Handler {
    notify: Notify,
    ui_thread: Option<ThreadId>,
}

static HANDLER: RwLock<Option<Handler>> = RwLock::new(None);

/// Enables certificate prompts. `notify` is called (from a network thread)
/// whenever a new prompt is queued, e.g. to repaint the UI; connections made on
/// `ui_thread` never wait for an answer. Without a handler, untrusted
/// certificates simply fail.
pub fn set_prompt_handler(notify: impl Fn() + Send + Sync + 'static, ui_thread: Option<ThreadId>) {
    *HANDLER.write().unwrap_or_else(|e| e.into_inner()) = Some(Handler {
        notify: Arc::new(notify),
        ui_thread,
    });
}

/// Disables prompts; waiting connections fail.
pub fn clear_prompt_handler() {
    *HANDLER.write().unwrap_or_else(|e| e.into_inner()) = None;
    let mut p = prompts();
    let ids: Vec<u64> = p.pending.iter().map(|x| x.prompt.id).collect();
    p.pending.clear();
    let now = Instant::now();
    p.answered
        .extend(ids.into_iter().map(|id| (id, false, now)));
    drop(p);
    ANSWERED.notify_all();
}

/// Certificates waiting for a decision, oldest first.
pub fn pending_prompts() -> Vec<CertificatePrompt> {
    prompts().pending.iter().map(|p| p.prompt.clone()).collect()
}

/// Whether to reject, trust for this process, or persist the host-bound pin.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrustDecision {
    Reject,
    Session,
    Permanent,
}

/// Compatibility entry point: accepted certificates are persisted.
pub fn answer_prompt(id: u64, accept: bool) {
    if let Err(error) = answer_prompt_with(
        id,
        if accept {
            TrustDecision::Permanent
        } else {
            TrustDecision::Reject
        },
    ) {
        log::warn!("{error}");
    }
}
/// Answers the prompt and reports storage errors. A failed persistent write
/// falls back to a session pin, so the UI does not claim durable trust.
pub fn answer_prompt_with(id: u64, decision: TrustDecision) -> Result<(), String> {
    let entry = {
        let mut state = prompts();
        let Some(index) = state.pending.iter().position(|p| p.prompt.id == id) else {
            return Err("Certificate prompt has expired".into());
        };
        state.pending.remove(index)
    };
    let result = match decision {
        TrustDecision::Reject => Ok(()),
        TrustDecision::Session => CertStore::new(None)
            .add(&entry.der, &entry.prompt.host)
            .map_err(|e| e.to_string()),
        TrustDecision::Permanent => match entry.store.add(&entry.der, &entry.prompt.host) {
            Ok(()) => Ok(()),
            Err(error) => {
                match CertStore::new(None).add(&entry.der,&entry.prompt.host) {
                    Ok(())=>Err(format!("Certificate trusted for this session only; it could not be saved: {error}")),
                    Err(session_error)=>Err(format!("Certificate could not be trusted: {session_error}; saving also failed: {error}")),
                }
            }
        },
    };
    let accept = decision != TrustDecision::Reject
        && is_accepted_for(&entry.fingerprint, &entry.prompt.host);
    let now = Instant::now();
    let mut state = prompts();
    state
        .answered
        .retain(|(_, _, at)| now.duration_since(*at) < Duration::from_secs(600));
    state.answered.push((id, accept, now));
    let rejection = (entry.fingerprint, entry.prompt.host);
    if accept {
        state.rejected.remove(&rejection);
    } else {
        state.rejected.insert(rejection, now);
    }
    drop(state);
    ANSWERED.notify_all();
    result
}

/// Queues a prompt for `ee` and waits for the answer (unless on the UI thread).
fn ask(
    host: &str,
    problem: String,
    ee: &[u8],
    intermediates: &[CertificateDer<'_>],
    store: &CertStore,
) -> bool {
    let (notify, ui_thread) = match HANDLER.read().unwrap_or_else(|e| e.into_inner()).as_ref() {
        Some(h) => (h.notify.clone(), h.ui_thread),
        None => return false,
    };
    let fingerprint = sha256(ee);
    let on_ui_thread = ui_thread == Some(std::thread::current().id());
    let mut p = prompts();
    let rejection = (fingerprint, host.to_string());
    if let Some(at) = p.rejected.get(&rejection).copied() {
        if at.elapsed() < REJECTION_MEMORY {
            return false;
        }
        p.rejected.remove(&rejection);
    }
    let mut created = false;
    // One question per certificate and host: the answer pins it to that host.
    let id = match p
        .pending
        .iter_mut()
        .find(|x| x.fingerprint == fingerprint && x.prompt.host == host)
    {
        Some(x) => {
            if !on_ui_thread {
                x.waiters += 1;
                x.prompt.waiting = true;
            }
            x.prompt.id
        }
        None => {
            let id = p.next_id;
            p.next_id += 1;
            let chain = std::iter::once(ee)
                .chain(intermediates.iter().map(|c| c.as_ref()))
                .map(CertificateInfo::from_der)
                .collect();
            let prompt = CertificatePrompt {
                id,
                host: host.to_string(),
                problem,
                chain,
                waiting: !on_ui_thread,
            };
            p.pending.push(Pending {
                prompt,
                fingerprint,
                der: ee.to_vec(),
                store: store.clone(),
                waiters: usize::from(!on_ui_thread),
            });
            created = true;
            id
        }
    };
    if created {
        drop(p);
        notify();
        p = prompts();
    }
    if on_ui_thread {
        return false;
    }
    let deadline = Instant::now() + PROMPT_TIMEOUT;
    loop {
        if let Some((_, accepted, _)) = p.answered.iter().find(|(i, _, _)| *i == id) {
            return *accepted;
        }
        let Some(entry) = p.pending.iter_mut().find(|x| x.prompt.id == id) else {
            return false;
        };
        let now = Instant::now();
        if now >= deadline {
            entry.waiters = entry.waiters.saturating_sub(1);
            entry.prompt.waiting = entry.waiters > 0;
            log::info!("no answer about the certificate of {host}; the connection fails");
            return false;
        }
        p = ANSWERED
            .wait_timeout(p, deadline - now)
            .map(|(g, _)| g)
            .unwrap_or_else(|e| e.into_inner().0);
    }
}

/// `DnsName("a.test")` / `IpAddress(1.2.3.4)` (how rustls lists presented names) as plain text.
fn plain_name(name: &str) -> String {
    let name = name.trim();
    match (name.find('('), name.ends_with(')')) {
        (Some(open), true) => name[open + 1..name.len() - 1].trim_matches('"').to_string(),
        _ => name.to_string(),
    }
}

/// Why a certificate is not trusted, in words.
pub fn describe_problem(error: &CertificateError, host: &str) -> String {
    let date = |t: &UnixTime| format_time(t.as_secs() as i64)[..10].to_string();
    match error {
        CertificateError::UnknownIssuer => "The certificate is not issued by a trusted certificate authority: it is self-signed, signed by a private or corporate CA, or the server does not send its intermediate certificates.".into(),
        CertificateError::Expired => "The certificate has expired.".into(),
        CertificateError::ExpiredContext { not_after, .. } => format!("The certificate expired on {}.", date(not_after)),
        CertificateError::NotValidYet => "The certificate is not valid yet (check the system clock).".into(),
        CertificateError::NotValidYetContext { not_before, .. } => format!("The certificate is only valid from {} (check the system clock).", date(not_before)),
        CertificateError::NotValidForName => format!("The certificate is not issued for {host}."),
        CertificateError::NotValidForNameContext { presented, .. } => {
            if presented.is_empty() {
                format!("The certificate is not issued for {host}.")
            } else {
                format!("The certificate is issued for {}, not for {host}.", presented.iter().map(|n| plain_name(n)).collect::<Vec<_>>().join(", "))
            }
        }
        CertificateError::Revoked => "The certificate has been revoked.".into(),
        CertificateError::BadSignature => "The certificate's signature is invalid.".into(),
        CertificateError::InvalidPurpose | CertificateError::InvalidPurposeContext { .. } => "The certificate is not meant for TLS servers.".into(),
        CertificateError::BadEncoding => "The certificate is malformed.".into(),
        // webpki errors rustls has no variant for (their Display is the variant name).
        CertificateError::Other(other) => match other.to_string().as_str() {
            "CaUsedAsEndEntity" => "The server presents a certificate authority (CA) certificate as its own certificate, as self-signed certificates made with `openssl req -x509` are by default.".into(),
            "EndEntityUsedAsCa" => "A server certificate is used as a certificate authority in the chain.".into(),
            "PathLenConstraintViolated" | "NameConstraintViolation" => "The certificate chain breaks a constraint of an issuing certificate authority.".into(),
            "UnsupportedCertVersion" => "The certificate uses an unsupported X.509 version.".into(),
            "UnsupportedCriticalExtension" => "The certificate has a critical extension that cannot be checked.".into(),
            name => format!("The certificate cannot be verified ({name})."),
        },
        other => format!("The certificate cannot be verified ({other:?})."),
    }
}

/// WebPKI verification plus accepted certificates and prompts.
pub(crate) struct ConfirmingVerifier {
    pub(crate) inner: Arc<WebPkiServerVerifier>,
    pub(crate) store: CertStore,
    pub(crate) accept_automatically: bool,
}

impl fmt::Debug for ConfirmingVerifier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ConfirmingVerifier")
            .field("store", &self.store)
            .field("accept_automatically", &self.accept_automatically)
            .finish()
    }
}

impl ServerCertVerifier for ConfirmingVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        ocsp_response: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, Error> {
        let error = match self.inner.verify_server_cert(
            end_entity,
            intermediates,
            server_name,
            ocsp_response,
            now,
        ) {
            Ok(ok) => return Ok(ok),
            Err(e) => e,
        };
        let Error::InvalidCertificate(cert_error) = &error else {
            return Err(error);
        };
        let host = server_name.to_str().into_owned();
        if is_accepted_for(&sha256(end_entity.as_ref()), &host) {
            return Ok(ServerCertVerified::assertion());
        }
        if self.accept_automatically {
            log::warn!("accepting the untrusted certificate of {host} automatically: {error}");
            if let Err(e) = self.store.add(end_entity.as_ref(), &host) {
                log::warn!("cannot store the certificate of {host}: {e}");
            }
            return Ok(ServerCertVerified::assertion());
        }
        let problem = describe_problem(cert_error, &host);
        if ask(
            &host,
            problem,
            end_entity.as_ref(),
            intermediates,
            &self.store,
        ) {
            Ok(ServerCertVerified::assertion())
        } else {
            Err(error)
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        self.inner.verify_tls12_signature(message, cert, dss)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        self.inner.verify_tls13_signature(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.inner.supported_verify_schemes()
    }
}

/// Forgets pending prompts and rejections (tests share the process-wide state).
#[cfg(test)]
pub(crate) fn tests_reset() {
    let mut p = prompts();
    p.pending.clear();
    p.rejected.clear();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn problems_in_words() {
        assert!(describe_problem(&CertificateError::UnknownIssuer, "h").contains("self-signed"));
        let ctx = CertificateError::NotValidForNameContext {
            expected: ServerName::try_from("a.test").unwrap(),
            presented: vec![
                "DnsName(\"b.test\")".into(),
                "c.test".into(),
                "IpAddress(10.0.0.1)".into(),
            ],
        };
        assert_eq!(
            describe_problem(&ctx, "a.test"),
            "The certificate is issued for b.test, c.test, 10.0.0.1, not for a.test."
        );
        let expired = CertificateError::ExpiredContext {
            time: UnixTime::since_unix_epoch(Duration::from_secs(2_000_000_000)),
            not_after: UnixTime::since_unix_epoch(Duration::from_secs(1_791_633_600)),
        };
        assert_eq!(
            describe_problem(&expired, "h"),
            "The certificate expired on 2026-10-10."
        );
        // webpki errors that reach us wrapped in `Other`.
        #[derive(Debug)]
        struct Named(&'static str);
        impl fmt::Display for Named {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(self.0)
            }
        }
        impl std::error::Error for Named {}
        let other = |name| CertificateError::Other(rustls::OtherError(Arc::new(Named(name))));
        assert!(describe_problem(&other("CaUsedAsEndEntity"), "h")
            .contains("certificate authority (CA) certificate as its own"));
        assert_eq!(
            describe_problem(&other("Novel"), "h"),
            "The certificate cannot be verified (Novel)."
        );
    }
}

#[cfg(test)]
mod decision_fixtures {
    use super::*;
    fn der() -> Vec<u8> {
        crate::certs::pem_certificates(crate::test_certs::SELF_SIGNED_PEM.as_bytes()).remove(0)
    }
    fn pending(id: u64, host: &str, store: CertStore) {
        let der = der();
        prompts().pending.push(Pending {
            prompt: CertificatePrompt {
                id,
                host: host.into(),
                problem: "untrusted".into(),
                chain: vec![CertificateInfo::from_der(&der)],
                waiting: true,
            },
            fingerprint: sha256(&der),
            der,
            store,
            waiters: 1,
        });
    }
    #[test]
    fn session_pin_stays_scoped_when_another_host_is_saved() {
        let _guard = crate::test_lock();
        tests_reset();
        let dir = tempfile::tempdir().unwrap();
        let store = CertStore::new(Some(dir.path().join("certs")));
        let der = der();
        let fingerprint = sha256(&der);
        store.remove(&fingerprint).unwrap();
        pending(u64::MAX - 1, "session.test", store.clone());
        answer_prompt_with(u64::MAX - 1, TrustDecision::Session).unwrap();
        pending(u64::MAX - 2, "permanent.test", store.clone());
        answer_prompt_with(u64::MAX - 2, TrustDecision::Permanent).unwrap();
        assert!(is_accepted_for(&fingerprint, "session.test"));
        assert!(is_accepted_for(&fingerprint, "permanent.test"));
        assert_eq!(
            crate::certs::session_hosts(&fingerprint),
            vec!["session.test"]
        );
        let stored = store
            .list()
            .into_iter()
            .find(|c| c.path.is_some() && c.info.sha256 == fingerprint)
            .unwrap();
        let text = std::fs::read_to_string(stored.path.unwrap()).unwrap();
        assert!(text.contains("permanent.test"));
        assert!(!text.contains("session.test"));
        store.remove(&fingerprint).unwrap();
        tests_reset();
    }
    #[test]
    fn failed_save_is_session_only_and_the_waiter_gets_an_answer() {
        let _guard = crate::test_lock();
        tests_reset();
        let file = tempfile::NamedTempFile::new().unwrap();
        let store = CertStore::new(Some(file.path().to_owned()));
        let fingerprint = sha256(&der());
        CertStore::new(None).remove(&fingerprint).unwrap();
        pending(u64::MAX - 3, "fallback.test", store);
        let error = answer_prompt_with(u64::MAX - 3, TrustDecision::Permanent).unwrap_err();
        assert!(error.contains("session only"));
        assert!(is_accepted_for(&fingerprint, "fallback.test"));
        assert!(prompts()
            .answered
            .iter()
            .any(|(id, accepted, _)| *id == u64::MAX - 3 && *accepted));
        CertStore::new(None).remove(&fingerprint).unwrap();
        tests_reset();
    }
}

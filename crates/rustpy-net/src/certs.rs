//! Server certificates the user accepted (Tools › Server Certificates) and the
//! operating system's CA bundle.
//!
//! Accepted certificates are PEM files in `<config dir>/certs`, one per
//! certificate, named after the start of the SHA-256 fingerprint, with comment
//! lines saying when and how the certificate is trusted. They are trusted in
//! one of two ways:
//!
//! * **For a server** (`# Host: example.com` lines): a certificate accepted when
//!   a prompt asked about it. Exactly this certificate is then accepted for
//!   exactly these hosts (pinning), like a browser's certificate exception. It
//!   is not a certificate authority: accepting a rogue server's certificate
//!   lets it impersonate only that server, not every other one.
//! * **As a certificate authority** (`# Trust: certificate authority`, and any
//!   PEM or DER file put into the folder by hand): an imported certificate is a
//!   trust anchor, so the certificates it issued are trusted for the names they
//!   are issued for — e.g. a company's root CA.

use std::collections::{HashMap, HashSet};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock, RwLock};

use base64::Engine as _;
use ureq::rustls::pki_types::pem::PemObject;
use ureq::rustls::pki_types::CertificateDer;

use crate::x509::{format_time, sha256, CertificateInfo};

/// Largest certificate file read from the certificates folder.
const MAX_FILE: u64 = 4 * 1024 * 1024;

/// All `CERTIFICATE` blocks of a PEM text, as DER.
pub fn pem_certificates(data: &[u8]) -> Vec<Vec<u8>> {
    CertificateDer::pem_slice_iter(data)
        .filter_map(Result::ok)
        .map(|c| c.as_ref().to_vec())
        .collect()
}

/// Certificates in a file's contents: PEM blocks, or a single DER certificate.
pub fn read_certificates(data: &[u8]) -> Vec<Vec<u8>> {
    let pem = pem_certificates(data);
    if !pem.is_empty() {
        return pem;
    }
    // DER: a SEQUENCE that parses as a certificate.
    if data.first() == Some(&0x30) && CertificateInfo::from_der(data).parse_error.is_none() {
        return vec![data.to_vec()];
    }
    Vec::new()
}

/// PEM encoding of a DER certificate (64-character lines).
pub fn to_pem(der: &[u8]) -> String {
    let b64 = base64::engine::general_purpose::STANDARD.encode(der);
    let mut out = String::from("-----BEGIN CERTIFICATE-----\n");
    for chunk in b64.as_bytes().chunks(64) {
        out.push_str(std::str::from_utf8(chunk).unwrap_or_default());
        out.push('\n');
    }
    out.push_str("-----END CERTIFICATE-----\n");
    out
}

/// An accepted certificate.
#[derive(Clone, Debug)]
pub struct AcceptedCertificate {
    /// The file it is stored in (None: accepted for this session only).
    pub path: Option<PathBuf>,
    pub der: Vec<u8>,
    pub info: CertificateInfo,
    /// "Accepted for example.com on 2026-10-10" (from the file's comments).
    pub note: String,
    /// Servers (host names or IP addresses, lower case) this exact certificate
    /// is accepted for.
    pub hosts: Vec<String>,
    /// Trusted as a certificate authority (a trust anchor).
    pub anchor: bool,
}

const HOST_PREFIX: &str = "Host:";
const ANCHOR_LINE: &str = "Trust: certificate authority";

/// Certificates accepted while no certificates folder is configured.
/// Hosts accepted only for this process (never promoted by another host's save).
pub fn session_hosts(fingerprint: &[u8; 32]) -> Vec<String> {
    SESSION
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .filter(|c| &c.info.sha256 == fingerprint)
        .flat_map(|c| c.hosts.clone())
        .collect()
}

static SESSION: Mutex<Vec<AcceptedCertificate>> = Mutex::new(Vec::new());
/// Pinned certificates: SHA-256 fingerprint → the hosts it is accepted for (folder + session).
static PINNED: RwLock<Option<HashMap<[u8; 32], HashSet<String>>>> = RwLock::new(None);

/// Whether the certificate with this SHA-256 fingerprint was accepted for `host`.
pub(crate) fn is_accepted_for(fingerprint: &[u8; 32], host: &str) -> bool {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    PINNED
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .and_then(|m| m.get(fingerprint))
        .is_some_and(|hosts| hosts.contains(&host))
}

/// Reloads the pinned certificates from `store`.
pub(crate) fn refresh_accepted(store: &CertStore) {
    let map = store
        .list()
        .into_iter()
        .filter(|c| !c.hosts.is_empty())
        .map(|c| (c.info.sha256, c.hosts.into_iter().collect()))
        .collect();
    *PINNED.write().unwrap_or_else(|e| e.into_inner()) = Some(map);
}

/// The comment lines of a certificate file (`# …`, trimmed).
fn comments(text: &str) -> impl Iterator<Item = &str> {
    text.lines()
        .filter_map(|l| l.trim_start().strip_prefix('#'))
        .map(str::trim)
}

fn note_of(text: &str) -> String {
    comments(text)
        .find(|l| l.starts_with("Accepted") || l.starts_with("Imported"))
        .unwrap_or_default()
        .to_string()
}

/// Hosts of `# Host:` lines, and whether the file is a trust anchor: marked as
/// one, or a plain certificate file without host lines (put there by hand).
fn trust_of(text: &str) -> (Vec<String>, bool) {
    let mut hosts: Vec<String> = Vec::new();
    let mut marked = false;
    for c in comments(text) {
        if let Some(h) = c.strip_prefix(HOST_PREFIX) {
            let h = h.trim().trim_end_matches('.').to_ascii_lowercase();
            if !h.is_empty() && !hosts.contains(&h) {
                hosts.push(h);
            }
        } else if c == ANCHOR_LINE {
            marked = true;
        }
    }
    let anchor = marked || hosts.is_empty();
    (hosts, anchor)
}

/// The file written for an accepted certificate.
fn certificate_file(der: &[u8], note: &str, hosts: &[String], anchor: bool) -> String {
    let info = CertificateInfo::from_der(der);
    let mut text = format!("# {note}\n");
    for h in hosts {
        text.push_str(&format!("# {HOST_PREFIX} {h}\n"));
    }
    if anchor {
        text.push_str(&format!("# {ANCHOR_LINE}\n"));
    }
    text.push_str(&format!(
        "# Subject: {}\n# Issuer: {}\n# SHA-256: {}\n{}",
        info.subject,
        info.issuer,
        info.sha256_hex(),
        to_pem(der)
    ));
    text
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Accepted certificates: a folder (`<config dir>/certs`), or memory only.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CertStore {
    dir: Option<PathBuf>,
}

impl CertStore {
    pub fn new(dir: Option<PathBuf>) -> Self {
        CertStore { dir }
    }

    pub fn dir(&self) -> Option<&Path> {
        self.dir.as_deref()
    }

    fn files(&self) -> Vec<PathBuf> {
        let Some(dir) = &self.dir else {
            return Vec::new();
        };
        let Ok(rd) = std::fs::read_dir(dir) else {
            return Vec::new();
        };
        let mut files: Vec<PathBuf> = rd
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| {
                p.is_file()
                    && matches!(
                        p.extension()
                            .and_then(|e| e.to_str())
                            .map(|e| e.to_ascii_lowercase())
                            .as_deref(),
                        Some("pem" | "crt" | "cer" | "der")
                    )
            })
            .collect();
        files.sort();
        files
    }

    /// Every accepted certificate, sorted by name. A certificate found in
    /// several files is listed once, with the hosts and trust of all of them.
    pub fn list(&self) -> Vec<AcceptedCertificate> {
        self.list_scoped(true)
    }

    fn list_scoped(&self, include_session: bool) -> Vec<AcceptedCertificate> {
        let mut out: Vec<AcceptedCertificate> = Vec::new();
        let mut merge = |cert: AcceptedCertificate| match out
            .iter_mut()
            .find(|c| c.info.sha256 == cert.info.sha256)
        {
            Some(existing) => {
                existing.anchor |= cert.anchor;
                for h in cert.hosts {
                    if !existing.hosts.contains(&h) {
                        existing.hosts.push(h);
                    }
                }
            }
            None => out.push(cert),
        };
        for path in self.files() {
            if std::fs::metadata(&path)
                .map(|m| m.len() > MAX_FILE)
                .unwrap_or(true)
            {
                continue;
            }
            let Ok(data) = std::fs::read(&path) else {
                continue;
            };
            let text = String::from_utf8_lossy(&data);
            let note = note_of(&text);
            let (hosts, anchor) = trust_of(&text);
            for der in read_certificates(&data) {
                merge(AcceptedCertificate {
                    path: Some(path.clone()),
                    info: CertificateInfo::from_der(&der),
                    der,
                    note: note.clone(),
                    hosts: hosts.clone(),
                    anchor,
                });
            }
        }
        if include_session {
            for cert in SESSION.lock().unwrap_or_else(|e| e.into_inner()).iter() {
                merge(cert.clone());
            }
        }
        out.sort_by_key(|c| c.info.display_name().to_lowercase());
        out
    }

    /// DER of the certificates trusted as certificate authorities.
    pub fn anchors(&self) -> Vec<Vec<u8>> {
        self.list()
            .into_iter()
            .filter(|c| c.anchor)
            .map(|c| c.der)
            .collect()
    }

    /// Accepts a certificate: for the server `host` (pinned to it), or with an
    /// empty `host` as a certificate authority. Idempotent; a certificate
    /// accepted before keeps its other hosts and trust.
    pub fn add(&self, der: &[u8], host: &str) -> io::Result<()> {
        let host = host.trim().trim_end_matches('.').to_ascii_lowercase();
        let fp = sha256(der);
        // Persisting one host must never promote another host's session-only
        // decision to permanent trust for the same certificate.
        let existing = self
            .list_scoped(self.dir.is_none())
            .into_iter()
            .find(|c| c.info.sha256 == fp);
        let mut hosts = existing
            .as_ref()
            .map(|c| c.hosts.clone())
            .unwrap_or_default();
        let mut anchor = existing.as_ref().is_some_and(|c| c.anchor);
        if host.is_empty() {
            anchor = true;
        } else if !hosts.contains(&host) {
            hosts.push(host.clone());
        }
        let unchanged = existing
            .as_ref()
            .is_some_and(|c| c.anchor == anchor && c.hosts == hosts);
        if !unchanged {
            let date = &format_time(now_unix())[..10];
            let note = match existing
                .as_ref()
                .map(|c| c.note.clone())
                .filter(|n| !n.is_empty())
            {
                Some(n) => n,
                None if host.is_empty() => format!("Imported on {date}"),
                None => format!("Accepted for {host} on {date}"),
            };
            match &self.dir {
                Some(dir) => {
                    std::fs::create_dir_all(dir)?;
                    let hex: String = fp.iter().take(8).map(|b| format!("{b:02x}")).collect();
                    let path = dir.join(format!("{hex}.pem"));
                    let tmp = dir.join(format!(".{hex}.pem.tmp"));
                    std::fs::write(&tmp, certificate_file(der, &note, &hosts, anchor))?;
                    std::fs::rename(&tmp, &path)?;
                }
                None => {
                    let mut session = SESSION.lock().unwrap_or_else(|e| e.into_inner());
                    session.retain(|c| c.info.sha256 != fp);
                    let note = if note.ends_with("(this session only)") {
                        note
                    } else {
                        format!("{note} (this session only)")
                    };
                    session.push(AcceptedCertificate {
                        path: None,
                        der: der.to_vec(),
                        info: CertificateInfo::from_der(der),
                        note,
                        hosts: hosts.clone(),
                        anchor,
                    });
                }
            }
        }
        if self.dir.is_some() {
            let mut session = SESSION.lock().unwrap_or_else(|e| e.into_inner());
            for cert in session.iter_mut().filter(|c| c.info.sha256 == fp) {
                cert.hosts.retain(|h| !anchor && !hosts.contains(h));
            }
            session.retain(|c| c.info.sha256 != fp || !c.hosts.is_empty());
        }
        if self.dir.is_some() {
            refresh_accepted(self);
        } else if !hosts.is_empty() {
            PINNED
                .write()
                .unwrap_or_else(|e| e.into_inner())
                .get_or_insert_with(HashMap::new)
                .entry(fp)
                .or_default()
                .extend(hosts);
        }
        crate::bump_generation();
        Ok(())
    }

    /// Imports the certificates of a PEM/DER file as certificate authorities;
    /// returns how many were added.
    pub fn import_file(&self, path: &Path) -> Result<usize, String> {
        let meta =
            std::fs::metadata(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        if meta.len() > MAX_FILE {
            return Err(format!(
                "{} is too large for a certificate file",
                path.display()
            ));
        }
        let data =
            std::fs::read(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        let ders = read_certificates(&data);
        if ders.is_empty() {
            return Err(format!(
                "{} contains no PEM or DER certificate",
                path.display()
            ));
        }
        for der in &ders {
            self.add(der, "")
                .map_err(|e| format!("cannot store the certificate: {e}"))?;
        }
        Ok(ders.len())
    }

    /// Removes the certificate with this fingerprint (its file is deleted, or the
    /// certificate is dropped from a multi-certificate file).
    pub fn remove(&self, fingerprint: &[u8; 32]) -> io::Result<()> {
        for path in self.files() {
            let Ok(data) = std::fs::read(&path) else {
                continue;
            };
            let ders = read_certificates(&data);
            if !ders.iter().any(|d| sha256(d) == *fingerprint) {
                continue;
            }
            let rest: Vec<&Vec<u8>> = ders.iter().filter(|d| sha256(d) != *fingerprint).collect();
            if rest.is_empty() {
                std::fs::remove_file(&path)?;
            } else {
                // Keep the comment lines: they say how the other certificates are trusted.
                let header: String = String::from_utf8_lossy(&data)
                    .lines()
                    .filter(|l| l.trim_start().starts_with('#'))
                    .map(|l| format!("{l}\n"))
                    .collect();
                let text: String = header + &rest.iter().map(|d| to_pem(d)).collect::<String>();
                std::fs::write(&path, text)?;
            }
        }
        SESSION
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|c| c.info.sha256 != *fingerprint);
        if let Some(map) = PINNED.write().unwrap_or_else(|e| e.into_inner()).as_mut() {
            map.remove(fingerprint);
        }
        crate::bump_generation();
        Ok(())
    }
}

/// Bundle files of the common Linux distributions, BSDs and macOS.
const SYSTEM_BUNDLES: &[&str] = &[
    "/etc/ssl/certs/ca-certificates.crt",
    "/etc/pki/tls/certs/ca-bundle.crt",
    "/etc/pki/ca-trust/extracted/pem/tls-ca-bundle.pem",
    "/etc/ssl/ca-bundle.pem",
    "/etc/pki/tls/cacert.pem",
    "/etc/ssl/cert.pem",
    "/usr/local/etc/ssl/cert.pem",
    "/usr/local/share/certs/ca-root-nss.crt",
];

/// What the operating system's certificate authorities are on this system
/// (Tools › Server Certificates).
pub const SYSTEM_STORE_DESCRIPTION: &str = crate::system_store::DESCRIPTION;

/// The operating system's CA certificates (read once): `SSL_CERT_FILE`, else
/// the first bundle file found, plus the files of `SSL_CERT_DIR`, plus the
/// roots of the Windows certificate store or the ones the macOS keychain trust
/// settings trust ([`crate::system_store`]).
pub fn system_certificates() -> &'static [Vec<u8>] {
    static CERTS: OnceLock<Vec<Vec<u8>>> = OnceLock::new();
    CERTS.get_or_init(|| {
        let env = |n: &str| std::env::var_os(n).filter(|v| !v.is_empty());
        let file = env("SSL_CERT_FILE").map(PathBuf::from).or_else(|| {
            SYSTEM_BUNDLES
                .iter()
                .map(PathBuf::from)
                .find(|p| p.is_file())
        });
        let mut out = Vec::new();
        if let Some(f) = file {
            if let Ok(data) = std::fs::read(&f) {
                out.extend(pem_certificates(&data));
            }
        }
        if let Some(dirs) = env("SSL_CERT_DIR") {
            for dir in std::env::split_paths(&dirs) {
                let Ok(rd) = std::fs::read_dir(&dir) else {
                    continue;
                };
                for entry in rd.flatten() {
                    let p = entry.path();
                    if p.is_file() && std::fs::metadata(&p).is_ok_and(|m| m.len() <= MAX_FILE) {
                        if let Ok(data) = std::fs::read(&p) {
                            out.extend(read_certificates(&data));
                        }
                    }
                }
            }
        }
        let platform = crate::system_store::platform_certificates();
        log::debug!(
            "{} system CA certificates ({} from the platform's store)",
            out.len() + platform.len(),
            platform.len()
        );
        out.extend(platform);
        out
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_certs::*;

    #[test]
    fn pem_round_trip_and_der_files() {
        let der = pem_to_der(SELF_SIGNED_PEM);
        let pem = to_pem(&der);
        assert!(pem.lines().all(|l| l.len() <= 64));
        assert_eq!(pem_certificates(pem.as_bytes()), vec![der.clone()]);
        assert_eq!(read_certificates(&der), vec![der.clone()]);
        let bundle = format!("junk\n{CA_PEM}\n# comment\n{LEAF_PEM}");
        assert_eq!(read_certificates(bundle.as_bytes()).len(), 2);
        assert!(read_certificates(b"\x30\x03\x02\x01\x00").is_empty());
        assert!(read_certificates(b"hello").is_empty());
    }

    #[test]
    fn folder_store_add_list_remove() {
        let _g = crate::test_lock();
        let dir = tempfile::tempdir().unwrap();
        let store = CertStore::new(Some(dir.path().join("certs")));
        assert!(store.list().is_empty());
        let der = pem_to_der(SELF_SIGNED_PEM);
        let fp = sha256(&der);
        store.add(&der, "Self.Test.").unwrap();
        store.add(&der, "self.test").unwrap();
        let list = store.list();
        assert_eq!(list.len(), 1, "accepting twice keeps one file");
        assert!(
            list[0].note.starts_with("Accepted for self.test on "),
            "{}",
            list[0].note
        );
        assert_eq!(list[0].info.subject.common_name(), Some("localhost"));
        assert_eq!(
            (list[0].hosts.clone(), list[0].anchor),
            (vec!["self.test".to_string()], false),
            "pinned to its host, not a CA"
        );
        assert!(is_accepted_for(&fp, "self.test") && is_accepted_for(&fp, "SELF.test."));
        assert!(
            !is_accepted_for(&fp, "localhost"),
            "another name is not covered"
        );
        assert!(store.anchors().is_empty());
        let file = std::fs::read_to_string(list[0].path.as_ref().unwrap()).unwrap();
        assert!(
            file.contains("# Host: self.test\n")
                && file.contains("# SHA-256: ")
                && file.contains("BEGIN CERTIFICATE"),
            "{file}"
        );
        // Accepted for a second host: one file, both hosts.
        store.add(&der, "127.0.0.1").unwrap();
        assert_eq!(
            store.list()[0].hosts,
            vec!["self.test".to_string(), "127.0.0.1".to_string()]
        );
        assert!(is_accepted_for(&fp, "127.0.0.1"));
        // The pins survive a restart (reload from the folder).
        *PINNED.write().unwrap() = None;
        refresh_accepted(&store);
        assert!(is_accepted_for(&fp, "self.test") && is_accepted_for(&fp, "127.0.0.1"));
        // Importing it as well makes it a certificate authority and keeps the hosts.
        store.add(&der, "").unwrap();
        let list = store.list();
        assert!(
            list[0].anchor && list[0].hosts.len() == 2,
            "{:?}",
            (list[0].anchor, &list[0].hosts)
        );
        assert_eq!(store.anchors(), vec![der.clone()]);
        // A bundle file put into the folder by hand: certificate authorities.
        std::fs::write(
            dir.path().join("certs/corp.crt"),
            format!("# Company roots\n{CA_PEM}{LEAF_PEM}"),
        )
        .unwrap();
        assert_eq!(store.list().len(), 3);
        assert!(store.list().iter().all(|c| c.anchor));
        let ca = pem_to_der(CA_PEM);
        store.remove(&sha256(&ca)).unwrap();
        let names: Vec<String> = store.list().iter().map(|c| c.info.display_name()).collect();
        assert_eq!(
            names,
            vec!["leaf.test".to_string(), "localhost".to_string()]
        );
        assert!(
            std::fs::read_to_string(dir.path().join("certs/corp.crt"))
                .unwrap()
                .starts_with("# Company roots\n"),
            "comments are kept"
        );
        store.remove(&fp).unwrap();
        assert!(!is_accepted_for(&fp, "self.test"));
        assert_eq!(store.list().len(), 1);
        // Import.
        let src = dir.path().join("rsa.pem");
        std::fs::write(&src, RSA_PEM).unwrap();
        assert_eq!(store.import_file(&src).unwrap(), 1);
        let rsa = store
            .list()
            .into_iter()
            .find(|c| c.info.subject.common_name() == Some("rsa.test"))
            .unwrap();
        assert!(
            rsa.anchor && rsa.hosts.is_empty() && rsa.note.starts_with("Imported on "),
            "{rsa:?}"
        );
        assert!(store.import_file(&dir.path().join("missing.pem")).is_err());
        std::fs::write(dir.path().join("empty.pem"), "nothing").unwrap();
        assert!(store
            .import_file(&dir.path().join("empty.pem"))
            .unwrap_err()
            .contains("no PEM"));
    }

    #[test]
    fn trust_comments() {
        assert_eq!(
            trust_of("# Host: a.test\n# Host: B.test.\n-----BEGIN"),
            (vec!["a.test".to_string(), "b.test".to_string()], false)
        );
        assert_eq!(
            trust_of("# Host: a.test\n# Trust: certificate authority\n"),
            (vec!["a.test".to_string()], true)
        );
        assert_eq!(
            trust_of("-----BEGIN CERTIFICATE-----"),
            (Vec::<String>::new(), true),
            "plain files are CAs"
        );
        assert_eq!(
            note_of("# Imported on 2026-10-10\n# Subject: x"),
            "Imported on 2026-10-10"
        );
    }

    #[test]
    fn session_store_without_folder() {
        let _g = crate::test_lock();
        let store = CertStore::new(None);
        let der = pem_to_der(RSA_PEM);
        store.add(&der, "rsa.test").unwrap();
        let list = store.list();
        let entry = list.iter().find(|c| c.der == der).unwrap();
        assert!(entry.path.is_none() && entry.note.contains("this session only"));
        assert!(is_accepted_for(&sha256(&der), "rsa.test"));
        store.remove(&sha256(&der)).unwrap();
        assert!(!store.list().iter().any(|c| c.der == der));
        assert!(!is_accepted_for(&sha256(&der), "rsa.test"));
    }
}

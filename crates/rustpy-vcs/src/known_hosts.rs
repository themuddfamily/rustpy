//! Read-only OpenSSH authority for the native Git transport. Never offers a
//! changed/revoked key as a first-use confirmation. New keys are session-only.
use base64::{engine::general_purpose::STANDARD, Engine};
use hmac::{Hmac, Mac};
use sha1::Sha1;
use std::{io::Read, path::Path};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Decision {
    Known,
    Unknown,
    Changed,
    Revoked,
}

pub(crate) fn endpoint(url: Option<&str>) -> Option<String> {
    let url = url?;
    if url.contains("://") {
        let parsed = url::Url::parse(url).ok()?;
        let host = parsed
            .host_str()?
            .trim_matches(['[', ']'])
            .to_ascii_lowercase();
        return Some(match parsed.port().unwrap_or(22) {
            22 => host,
            port => format!("[{host}]:{port}"),
        });
    }
    let host = url.split_once(':')?.0.rsplit('@').next()?;
    (!host.is_empty()).then(|| host.to_ascii_lowercase())
}

fn host_pattern(pattern: &str, host: &str) -> bool {
    if let Some(hash) = pattern.strip_prefix("|1|") {
        let Some((salt, expected)) = hash.split_once('|') else {
            return false;
        };
        let (Ok(salt), Ok(expected)) = (STANDARD.decode(salt), STANDARD.decode(expected)) else {
            return false;
        };
        let Ok(mut mac) = Hmac::<Sha1>::new_from_slice(&salt) else {
            return false;
        };
        mac.update(host.as_bytes());
        return mac.verify_slice(&expected).is_ok();
    }
    // OpenSSH wildcard host patterns, matched without exponential backtracking.
    let pattern = pattern.to_ascii_lowercase();
    let (p, h) = (pattern.as_bytes(), host.as_bytes());
    let (mut i, mut j, mut star, mut retry) = (0, 0, None, 0);
    while j < h.len() {
        if i < p.len() && (p[i] == b'?' || p[i] == h[j]) {
            i += 1;
            j += 1;
        } else if i < p.len() && p[i] == b'*' {
            star = Some(i);
            i += 1;
            retry = j;
        } else if let Some(s) = star {
            retry += 1;
            j = retry;
            i = s + 1;
        } else {
            return false;
        }
    }
    while i < p.len() && p[i] == b'*' {
        i += 1;
    }
    i == p.len()
}

fn matches_hosts(patterns: &str, host: &str) -> bool {
    let mut positive = false;
    for pattern in patterns.split(',') {
        if let Some(negated) = pattern.strip_prefix('!') {
            if host_pattern(negated, host) {
                return false;
            }
        } else {
            positive |= host_pattern(pattern, host);
        }
    }
    positive
}

fn inspect(text: &str, host: &str, algorithm: &str, key: &[u8]) -> Result<Decision, String> {
    let mut decision = Decision::Unknown;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut words = line.split_whitespace();
        let first = words.next().unwrap_or_default();
        let (marker, hosts) = if first.starts_with('@') {
            (first, words.next().unwrap_or_default())
        } else {
            ("", first)
        };
        if !matches_hosts(hosts, host) {
            continue;
        }
        let kind = words
            .next()
            .ok_or("Malformed known_hosts entry for this host")?;
        let encoded = words.next().ok_or("Missing known_hosts public key")?;
        let saved = STANDARD
            .decode(encoded)
            .map_err(|_| "Invalid known_hosts public key")?;
        if marker == "@revoked" && saved == key {
            return Ok(Decision::Revoked);
        }
        if marker == "@cert-authority" {
            // Raw host keys are not host certificates. Do not silently bypass
            // an administrator's certificate-only authority via a TOFU prompt.
            return Err("Native SSH cannot validate this host's certificate authority; select OpenSSH transport".into());
        }
        if !marker.is_empty() && marker != "@revoked" {
            return Err("Unsupported known_hosts marker".into());
        }
        if kind == algorithm && saved == key && marker != "@revoked" {
            decision = Decision::Known;
        } else if decision != Decision::Known {
            decision = Decision::Changed;
        }
    }
    Ok(decision)
}

fn read(path: &Path, host: &str, algorithm: &str, key: &[u8]) -> Result<Decision, String> {
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Decision::Unknown),
        Err(error) => return Err(format!("Cannot read {}: {error}", path.display())),
    };
    let mut bytes = Vec::new();
    file.take(1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.len() > 1024 * 1024 {
        return Err(
            "known_hosts exceeds the native transport size limit; select OpenSSH transport".into(),
        );
    }
    let text = std::str::from_utf8(&bytes)
        .map_err(|_| "known_hosts is not UTF-8; select OpenSSH transport")?;
    inspect(text, host, algorithm, key)
}

pub(crate) fn check(host: &str, algorithm: &str, key: &[u8]) -> Result<Decision, String> {
    let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .map(std::path::PathBuf::from)
        .filter(|home| home.is_absolute())
        .ok_or("Cannot locate SSH known_hosts; select OpenSSH transport")?;
    let mut combined = Decision::Unknown;
    let mut files = vec![
        home.join(".ssh/known_hosts"),
        home.join(".ssh/known_hosts2"),
    ];
    #[cfg(windows)]
    let system = std::env::var_os("PROGRAMDATA")
        .map(std::path::PathBuf::from)
        .filter(|path| path.is_absolute())
        .ok_or("Cannot locate system SSH known_hosts")?
        .join("ssh");
    #[cfg(not(windows))]
    let system = std::path::PathBuf::from("/etc/ssh");
    files.extend([
        system.join("ssh_known_hosts"),
        system.join("ssh_known_hosts2"),
    ]);
    for path in files {
        match read(&path, host, algorithm, key)? {
            Decision::Revoked => return Ok(Decision::Revoked),
            Decision::Known => combined = Decision::Known,
            Decision::Changed if combined != Decision::Known => combined = Decision::Changed,
            _ => {}
        }
    }
    Ok(combined)
}

#[cfg(test)]
mod fixtures {
    use super::*;
    #[test]
    fn changed_and_revoked_keys_cannot_be_first_use_prompts() {
        let text = "host ssh-ed25519 a2V5\n";
        assert_eq!(
            inspect(text, "host", "ssh-ed25519", b"key").unwrap(),
            Decision::Known
        );
        assert_eq!(
            inspect(text, "host", "ssh-ed25519", b"other").unwrap(),
            Decision::Changed
        );
        assert_eq!(
            inspect(text, "new-host", "ssh-ed25519", b"key").unwrap(),
            Decision::Unknown
        );
        assert_eq!(
            inspect(
                "@revoked host ssh-ed25519 a2V5",
                "host",
                "ssh-ed25519",
                b"key"
            )
            .unwrap(),
            Decision::Revoked
        );
        assert!(inspect(
            "@cert-authority host ssh-ed25519 a2V5",
            "host",
            "ssh-ed25519",
            b"key"
        )
        .is_err());
    }
    #[test]
    fn hashed_and_negated_hosts_and_nonstandard_ports_keep_their_authority() {
        let mut mac = Hmac::<Sha1>::new_from_slice(b"salt").unwrap();
        mac.update(b"[example.com]:2222");
        let hashed = format!(
            "|1|{}|{}",
            STANDARD.encode(b"salt"),
            STANDARD.encode(mac.finalize().into_bytes())
        );
        assert!(host_pattern(&hashed, "[example.com]:2222"));
        assert!(!host_pattern(&hashed, "example.com"));
        assert!(!matches_hosts(
            "*.example.com,!evil.example.com",
            "evil.example.com"
        ));
        assert!(matches_hosts(
            "*.example.com,!evil.example.com",
            "good.example.com"
        ));
        assert_eq!(
            endpoint(Some("ssh://git@example.com:2222/a")),
            Some("[example.com]:2222".into())
        );
        assert_eq!(
            endpoint(Some("git@example.com:a")),
            Some("example.com".into())
        );
    }
}

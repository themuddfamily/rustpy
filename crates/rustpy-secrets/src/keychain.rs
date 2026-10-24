//! macOS keyring: generic passwords in the login keychain through the
//! `security` command-line tool (service `RustPy`, account = key).
//!
//! Secrets never appear on a command line: they are written with
//! `security -i`, which reads its commands from stdin, and base64-encoded with
//! a `rustpy-base64:` prefix so that quoting and `security`'s hex output for
//! non-ASCII data cannot garble them (as go-keyring does). A write is verified
//! by reading the item back.

use std::io::Write as _;
use std::path::Path;
use std::process::{Command, Stdio};

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;

use crate::{Backend, SafeError};

const SECURITY: &str = "/usr/bin/security";
const SERVICE: &str = "RustPy";
const PREFIX: &str = "rustpy-base64:";
/// `security -i` reads lines of at most this length.
const MAX_COMMAND: usize = 4000;
/// Exit status of `security` when an item does not exist.
const NOT_FOUND: i32 = 44;

/// Single-quotes `s` for `security -i`'s shell-like command parser.
pub(crate) fn quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\"'\"'"))
}

pub(crate) fn encode(secret: &str) -> String {
    format!("{PREFIX}{}", B64.encode(secret.as_bytes()))
}

pub(crate) fn decode(stored: &str) -> Option<String> {
    match stored.strip_prefix(PREFIX) {
        Some(b64) => B64
            .decode(b64.trim())
            .ok()
            .and_then(|b| String::from_utf8(b).ok()),
        None => Some(stored.to_string()),
    }
}

/// Keys of `RustPy` generic passwords in `security dump-keychain` output.
pub(crate) fn parse_dump(text: &str) -> Vec<String> {
    let mut keys = Vec::new();
    let (mut class, mut service, mut account): (Option<String>, Option<String>, Option<String>) =
        (None, None, None);
    let mut flush =
        |class: &mut Option<String>, service: &mut Option<String>, account: &mut Option<String>| {
            if class.as_deref() == Some("genp") && service.as_deref() == Some(SERVICE) {
                if let Some(a) = account.take() {
                    keys.push(a);
                }
            }
            *class = None;
            *service = None;
            *account = None;
        };
    // `"acct"<blob>="value"` → value (quoted form only).
    let value_of = |line: &str| -> Option<String> {
        let (_, rest) = line.split_once("<blob>=")?;
        let rest = rest.trim();
        let start = rest.find('"')?;
        let end = rest.rfind('"')?;
        (end > start).then(|| rest[start + 1..end].to_string())
    };
    for line in text.lines() {
        let t = line.trim();
        if t.starts_with("keychain:") {
            flush(&mut class, &mut service, &mut account);
        } else if let Some(c) = t.strip_prefix("class:") {
            class = Some(c.trim().trim_matches('"').to_string());
        } else if t.starts_with("\"svce\"<blob>=") {
            service = value_of(t);
        } else if t.starts_with("\"acct\"<blob>=") {
            account = value_of(t);
        }
    }
    flush(&mut class, &mut service, &mut account);
    keys.sort();
    keys.dedup();
    keys
}

/// The macOS keychain.
pub struct Keychain;

impl Keychain {
    pub fn detect() -> Result<Keychain, String> {
        if Path::new(SECURITY).exists() {
            Ok(Keychain)
        } else {
            Err(format!("{SECURITY} is missing"))
        }
    }

    fn run(args: &[&str]) -> Result<std::process::Output, SafeError> {
        Command::new(SECURITY)
            .args(args)
            .stdin(Stdio::null())
            .output()
            .map_err(|e| SafeError::Failed(format!("cannot run {SECURITY}: {e}")))
    }
}

fn check_key(key: &str) -> Result<(), SafeError> {
    if key.chars().any(|c| c.is_control()) {
        return Err(SafeError::Failed("invalid password key".into()));
    }
    Ok(())
}

impl Backend for Keychain {
    fn name(&self) -> String {
        "macOS Keychain".into()
    }

    fn get(&self, key: &str) -> Result<Option<String>, SafeError> {
        check_key(key)?;
        let out = Self::run(&["find-generic-password", "-s", SERVICE, "-a", key, "-w"])?;
        match out.status.code() {
            Some(0) => {
                let text = String::from_utf8_lossy(&out.stdout);
                Ok(decode(text.trim_end_matches(['\n', '\r'])))
            }
            Some(NOT_FOUND) => Ok(None),
            _ => Err(SafeError::Failed(format!(
                "Keychain: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ))),
        }
    }

    fn set(&self, key: &str, secret: &str) -> Result<(), SafeError> {
        check_key(key)?;
        let line = format!(
            "add-generic-password -U -s {} -a {} -l {} -w {}\n",
            quote(SERVICE),
            quote(key),
            quote(&format!("RustPy: {key}")),
            quote(&encode(secret))
        );
        if line.len() > MAX_COMMAND {
            return Err(SafeError::Failed("Keychain: the secret is too long".into()));
        }
        let mut child = Command::new(SECURITY)
            .arg("-i")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| SafeError::Failed(format!("cannot run {SECURITY}: {e}")))?;
        if let Some(mut stdin) = child.stdin.take() {
            stdin
                .write_all(line.as_bytes())
                .map_err(|e| SafeError::Failed(format!("Keychain: {e}")))?;
        }
        let out = child
            .wait_with_output()
            .map_err(|e| SafeError::Failed(format!("Keychain: {e}")))?;
        if self.get(key)?.as_deref() != Some(secret) {
            return Err(SafeError::Failed(format!(
                "Keychain: the password was not stored ({})",
                String::from_utf8_lossy(&out.stderr).trim()
            )));
        }
        Ok(())
    }

    fn delete(&self, key: &str) -> Result<(), SafeError> {
        check_key(key)?;
        let out = Self::run(&["delete-generic-password", "-s", SERVICE, "-a", key])?;
        match out.status.code() {
            Some(0) | Some(NOT_FOUND) => Ok(()),
            _ => Err(SafeError::Failed(format!(
                "Keychain: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ))),
        }
    }

    fn keys(&self) -> Result<Vec<String>, SafeError> {
        let out = Self::run(&["dump-keychain"])?;
        if !out.status.success() {
            return Err(SafeError::Failed(format!(
                "Keychain: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            )));
        }
        Ok(parse_dump(&String::from_utf8_lossy(&out.stdout)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encoding_and_quoting() {
        for s in ["sk-ant-123", "pä$$ wörd", "it's \"quoted\"\nnext", ""] {
            let e = encode(s);
            assert!(
                e.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"+/=:-".contains(&b)),
                "{e}"
            );
            assert_eq!(decode(&e).as_deref(), Some(s));
        }
        assert_eq!(
            decode("plain from another tool").as_deref(),
            Some("plain from another tool")
        );
        assert_eq!(quote("it's"), r#"'it'"'"'s'"#);
        assert_eq!(
            quote("deployment/ftp://u@h:21"),
            "'deployment/ftp://u@h:21'"
        );
    }

    #[test]
    fn dump_parsing() {
        let dump = r#"keychain: "/Users/me/Library/Keychains/login.keychain-db"
version: 512
class: "genp"
attributes:
    0x00000007 <blob>="RustPy: ai-assistant/anthropic-api-key"
    "acct"<blob>="ai-assistant/anthropic-api-key"
    "svce"<blob>="RustPy"
keychain: "/Users/me/Library/Keychains/login.keychain-db"
version: 512
class: "genp"
attributes:
    "acct"<blob>="someone"
    "svce"<blob>="OtherApp"
keychain: "/Users/me/Library/Keychains/login.keychain-db"
class: "inet"
attributes:
    "acct"<blob>="x"
    "svce"<blob>="RustPy"
keychain: "/Users/me/Library/Keychains/login.keychain-db"
class: "genp"
attributes:
    "svce"<blob>="RustPy"
    "acct"<blob>="deployment/ftp://u@h:21"
"#;
        assert_eq!(
            parse_dump(dump),
            vec![
                "ai-assistant/anthropic-api-key".to_string(),
                "deployment/ftp://u@h:21".to_string()
            ]
        );
        assert!(parse_dump("").is_empty());
    }
}

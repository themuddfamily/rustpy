//! Optional Ed25519 publisher authentication, independent of archive checksums.
//! Authority is owned by IDE settings, never by plugin or project files.
use base64::{engine::general_purpose::STANDARD, Engine};
use ed25519_dalek::{Signature, VerifyingKey};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, io::Read, path::Path};

pub const SIGNATURE_FILE: &str = "plugin.signature.json";
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DetachedSignature {
    pub publisher: String,
    pub signature: String,
}

pub fn verify(
    payload: &[u8],
    signature: &DetachedSignature,
    publishers: &BTreeMap<String, String>,
) -> Result<(), String> {
    let public = publishers
        .get(&signature.publisher)
        .ok_or("Publisher is not trusted in IDE settings")?;
    let bytes: [u8; 32] = STANDARD
        .decode(public)
        .map_err(|_| "Invalid publisher public key")?
        .try_into()
        .map_err(|_| "Ed25519 public keys must be 32 bytes")?;
    let key = VerifyingKey::from_bytes(&bytes).map_err(|e| e.to_string())?;
    let signature = Signature::from_slice(
        &STANDARD
            .decode(&signature.signature)
            .map_err(|_| "Invalid detached signature")?,
    )
    .map_err(|e| e.to_string())?;
    key.verify_strict(payload, &signature)
        .map_err(|_| "Publisher signature verification failed".into())
}

fn bytes(path: &Path, limit: usize) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .map_err(|e| e.to_string())?
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > limit {
        return Err("Signed plugin exceeds its byte budget".into());
    }
    Ok(bytes)
}

/// Canonical content inventory: domain + length-prefixed UTF-8 relative paths
/// and SHA256 content digests, sorted by path. File mode and timestamps are not
/// signed; symbolic links, devices and paths outside the directory are refused.
pub fn inventory(root: &Path) -> Result<Vec<u8>, String> {
    let root = dunce::canonicalize(root).map_err(|e| e.to_string())?;
    let mut pending = vec![(root.clone(), 0)];
    let mut files = BTreeMap::new();
    let mut count = 0;
    let mut total = 0;
    while let Some((directory, depth)) = pending.pop() {
        if depth > 32 {
            return Err("Signed plugin nesting exceeds 32 levels".into());
        }
        for entry in std::fs::read_dir(directory).map_err(|e| e.to_string())? {
            let entry = entry.map_err(|e| e.to_string())?;
            count += 1;
            if count > 5000 {
                return Err("Signed plugin contains too many entries".into());
            }
            let path = entry.path();
            let kind = entry.file_type().map_err(|e| e.to_string())?;
            if kind.is_symlink() {
                return Err("Signed plugins cannot contain symbolic links".into());
            }
            if kind.is_dir() {
                pending.push((path, depth + 1));
                continue;
            }
            if !kind.is_file() {
                return Err("Signed plugins may contain only regular files".into());
            }
            let relative = path
                .strip_prefix(&root)
                .map_err(|e| e.to_string())?
                .to_str()
                .ok_or("Signed plugin paths must be UTF-8")?
                .replace('\\', "/");
            if relative == SIGNATURE_FILE {
                continue;
            }
            crate::paths::safe_relative_path(&relative)?;
            let file = crate::paths::existing_file_inside(&root, &relative)?;
            let data = bytes(&file, 8 * 1024 * 1024)?;
            total += data.len();
            if total > 64 * 1024 * 1024 {
                return Err("Signed plugin exceeds 64 MiB".into());
            }
            files.insert(relative, crate::sha256_hex(&data));
        }
    }
    let mut payload = b"RustPy plugin publisher signature v1\0".to_vec();
    for (path, digest) in files {
        payload.extend_from_slice(&(path.len() as u32).to_be_bytes());
        payload.extend_from_slice(path.as_bytes());
        payload.extend_from_slice(digest.as_bytes());
    }
    Ok(payload)
}

pub fn verify_directory(
    root: &Path,
    policy: &rustpy_settings::plugins::Settings,
) -> Result<Option<String>, String> {
    let path = root.join(SIGNATURE_FILE);
    if !path.exists() {
        if policy.require_signatures {
            return Err("This IDE requires a trusted publisher signature".into());
        }
        return Ok(None);
    }
    let path = crate::paths::existing_file_inside(root, SIGNATURE_FILE)?;
    let signature: DetachedSignature =
        serde_json::from_slice(&bytes(&path, 8192)?).map_err(|e| e.to_string())?;
    verify(&inventory(root)?, &signature, &policy.publishers)?;
    Ok(Some(signature.publisher))
}

/// A runtime signature must cover the bytes actually instantiated, not merely
/// a directory snapshot checked before a module or manifest file was replaced.
pub fn verify_loaded_runtime(
    root: &Path,
    module: &str,
    wasm: &[u8],
    manifest: &[u8],
    policy: &rustpy_settings::plugins::Settings,
) -> Result<(), String> {
    if !root.join(SIGNATURE_FILE).exists() {
        if policy.require_signatures {
            return Err("A trusted publisher signature is required".into());
        }
        return Ok(());
    }
    let signature: DetachedSignature = serde_json::from_slice(&bytes(
        &crate::paths::existing_file_inside(root, SIGNATURE_FILE)?,
        8192,
    )?)
    .map_err(|e| e.to_string())?;
    let inventory = inventory(root)?;
    verify(&inventory, &signature, &policy.publishers)?;
    let module = module
        .replace('\\', "/")
        .split('/')
        .filter(|part| !part.is_empty() && *part != ".")
        .collect::<Vec<_>>()
        .join("/");
    let wanted = [
        (module.as_str(), crate::sha256_hex(wasm)),
        (crate::MANIFEST_FILE, crate::sha256_hex(manifest)),
    ];
    let mut found = 0;
    let mut data = &inventory[b"RustPy plugin publisher signature v1\0".len()..];
    while !data.is_empty() {
        let length = u32::from_be_bytes(
            data.get(..4)
                .ok_or("Invalid signed inventory")?
                .try_into()
                .map_err(|_| "Invalid inventory length")?,
        ) as usize;
        let path = std::str::from_utf8(data.get(4..4 + length).ok_or("Invalid signed path")?)
            .map_err(|_| "Invalid signed path")?;
        let digest = data
            .get(4 + length..4 + length + 64)
            .ok_or("Invalid signed digest")?;
        for (wanted_path, wanted_digest) in &wanted {
            if path == *wanted_path {
                if digest != wanted_digest.as_bytes() {
                    return Err("Signed plugin changed while loading".into());
                }
                found += 1;
            }
        }
        data = &data[4 + length + 64..];
    }
    if found != 2 {
        return Err("Publisher signature does not cover the loaded runtime".into());
    }
    Ok(())
}

pub(crate) fn policy(config_dir: &Path) -> Result<rustpy_settings::plugins::Settings, String> {
    let path = config_dir.join("settings.json");
    if !path.exists() {
        return Ok(Default::default());
    }
    let settings: serde_json::Value =
        serde_json::from_slice(&bytes(&path, 8 * 1024 * 1024)?).map_err(|e| e.to_string())?;
    serde_json::from_value(
        settings
            .get("plugins")
            .cloned()
            .unwrap_or_else(|| serde_json::json!({})),
    )
    .map_err(|e| e.to_string())
}

#[cfg(test)]
mod fixtures {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};
    #[test]
    fn loaded_runtime_must_match_the_signed_inventory_not_an_earlier_snapshot() {
        let directory = tempfile::tempdir().unwrap();
        let wasm = b"signed wasm";
        let manifest = b"signed manifest";
        std::fs::write(directory.path().join("module.wasm"), wasm).unwrap();
        std::fs::write(directory.path().join(crate::MANIFEST_FILE), manifest).unwrap();
        let key = SigningKey::from_bytes(&[9; 32]);
        let signature = DetachedSignature {
            publisher: "fixture".into(),
            signature: STANDARD.encode(key.sign(&inventory(directory.path()).unwrap()).to_bytes()),
        };
        std::fs::write(
            directory.path().join(SIGNATURE_FILE),
            serde_json::to_vec(&signature).unwrap(),
        )
        .unwrap();
        let mut policy = rustpy_settings::plugins::Settings {
            require_signatures: true,
            ..Default::default()
        };
        policy.publishers.insert(
            "fixture".into(),
            STANDARD.encode(key.verifying_key().to_bytes()),
        );
        assert!(
            verify_loaded_runtime(directory.path(), "module.wasm", wasm, manifest, &policy).is_ok()
        );
        assert!(verify_loaded_runtime(
            directory.path(),
            "module.wasm",
            b"previous bytes",
            manifest,
            &policy
        )
        .is_err());
        assert!(verify_loaded_runtime(
            directory.path(),
            "module.wasm",
            wasm,
            b"previous manifest",
            &policy
        )
        .is_err());
    }
    #[test]
    fn signatures_require_user_publisher_authority_and_cover_payload_changes() {
        let key = SigningKey::from_bytes(&[7; 32]);
        let mut publishers = BTreeMap::new();
        let signature = DetachedSignature {
            publisher: "publisher".into(),
            signature: STANDARD.encode(key.sign(b"original").to_bytes()),
        };
        assert!(verify(b"original", &signature, &publishers).is_err());
        publishers.insert(
            "publisher".into(),
            STANDARD.encode(key.verifying_key().to_bytes()),
        );
        assert!(verify(b"original", &signature, &publishers).is_ok());
        assert!(verify(b"changed", &signature, &publishers).is_err());
    }
}

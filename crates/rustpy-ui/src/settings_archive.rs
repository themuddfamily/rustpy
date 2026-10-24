//! ZIP packaging of the existing validated Settings Sync manifest. Members are
//! never extracted to arbitrary paths; import retains Sync conflict/backup rules.
use rustpy_settings::{settings_sync as sync, Settings};
use std::{io::Write, path::Path};
const MEMBER: &str = "rustpy-settings.json";
fn crc(bytes: &[u8]) -> u32 {
    let mut value = !0u32;
    for byte in bytes {
        value ^= u32::from(*byte);
        for _ in 0..8 {
            value = (value >> 1) ^ (0xedb88320u32 & 0u32.wrapping_sub(value & 1));
        }
    }
    !value
}
fn word(out: &mut Vec<u8>, n: u16) {
    out.extend(n.to_le_bytes());
}
fn dword(out: &mut Vec<u8>, n: u32) {
    out.extend(n.to_le_bytes());
}
fn zip(bytes: &[u8]) -> Result<Vec<u8>, String> {
    if bytes.len() > 8 * 1024 * 1024 {
        return Err("Selected settings exceed the 8 MiB archive limit".into());
    }
    let mut out = vec![];
    out.extend(b"PK\x03\x04");
    for n in [20, 0x800, 0, 0, 0] {
        word(&mut out, n);
    }
    dword(&mut out, crc(bytes));
    dword(&mut out, bytes.len() as u32);
    dword(&mut out, bytes.len() as u32);
    word(&mut out, MEMBER.len() as u16);
    word(&mut out, 0);
    out.extend(MEMBER.as_bytes());
    out.extend(bytes);
    let directory = out.len() as u32;
    out.extend(b"PK\x01\x02");
    for n in [20, 20, 0x800, 0, 0, 0] {
        word(&mut out, n);
    }
    dword(&mut out, crc(bytes));
    dword(&mut out, bytes.len() as u32);
    dword(&mut out, bytes.len() as u32);
    for n in [MEMBER.len() as u16, 0, 0, 0, 0] {
        word(&mut out, n);
    }
    dword(&mut out, 0);
    dword(&mut out, 0);
    out.extend(MEMBER.as_bytes());
    let size = out.len() as u32 - directory;
    out.extend(b"PK\x05\x06");
    for n in [0, 0, 1, 1] {
        word(&mut out, n);
    }
    dword(&mut out, size);
    dword(&mut out, directory);
    word(&mut out, 0);
    Ok(out)
}
pub fn export(
    config: &Path,
    path: &Path,
    settings: &Settings,
    cancel: &impl Fn() -> bool,
) -> Result<String, String> {
    let snapshot = sync::capture(
        settings,
        config,
        &sync::Selection::default(),
        vec![],
        cancel,
    )
    .map_err(|e| e.to_string())?;
    let bytes = zip(sync::bundle(&snapshot)
        .map_err(|e| e.to_string())?
        .as_bytes())?;
    if cancel() {
        return Err("Export cancelled".into());
    }
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|e| e.to_string())?;
    let result = (|| {
        for chunk in bytes.chunks(65536) {
            if cancel() {
                return Err("Export cancelled".into());
            }
            file.write_all(chunk).map_err(|e| e.to_string())?;
        }
        file.sync_all().map_err(|e| e.to_string())
    })();
    if result.is_err() {
        drop(file);
        let _ = std::fs::remove_file(path);
    }
    result?;
    Ok(format!(
        "Exported {} selected settings entries to {}",
        snapshot.entries.len(),
        path.display()
    ))
}
pub fn import(config: &Path, path: &Path, cancel: &impl Fn() -> bool) -> Result<String, String> {
    let archive = crate::data_archive::Archive::open(path, 12 * 1024 * 1024)?;
    if archive.entries.len() != 1 || archive.entries[0].name != MEMBER {
        return Err("Choose a RustPy settings ZIP with one validated manifest".into());
    }
    let bytes = archive.member(0)?;
    let text = std::str::from_utf8(&bytes).map_err(|e| e.to_string())?;
    let remote = sync::from_bundle(text).map_err(|e| e.to_string())?;
    let (settings, _, error) = rustpy_settings::persist::load_from(&config.join("settings.json"));
    if let Some(error) = error {
        return Err(error.to_string());
    }
    let expected = sync::capture(
        &settings,
        config,
        &sync::Selection::default(),
        vec![],
        cancel,
    )
    .map_err(|e| e.to_string())?;
    let count = sync::apply(config, &remote, &expected, cancel)
        .map_err(|e| e.to_string())?
        .len();
    Ok(format!(
        "Imported {count} entries; existing files were backed up"
    ))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn archive_roundtrip_and_crc_are_real() {
        let bytes = zip(b"settings manifest").unwrap();
        let archive = crate::data_archive::Archive::parse(bytes).unwrap();
        assert_eq!(archive.entries.len(), 1);
        assert_eq!(archive.entries[0].name, MEMBER);
        assert_eq!(archive.member(0).unwrap(), b"settings manifest");
    }
    #[test]
    fn export_is_bounded() {
        assert!(zip(&vec![0; 8 * 1024 * 1024 + 1]).is_err());
    }
    #[test]
    fn corrupt_zip_payload_cannot_reach_settings_apply() {
        let mut bytes = zip(b"manifest").unwrap();
        bytes[30 + MEMBER.len()] ^= 1;
        let archive = crate::data_archive::Archive::parse(bytes).unwrap();
        assert!(archive.member(0).is_err());
    }
}

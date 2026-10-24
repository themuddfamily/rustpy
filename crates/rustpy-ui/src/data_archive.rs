//! Read-only ZIP central-directory browsing. No member is extracted implicitly.
use std::{io::Read, path::Path};
#[derive(Clone, Debug)]
pub struct Entry {
    pub name: String,
    pub compressed: u32,
    pub size: u32,
    pub method: u16,
    pub flags: u16,
    pub crc: u32,
    offset: usize,
    name_bytes: Vec<u8>,
    pub safe: bool,
}
pub struct Archive {
    pub entries: Vec<Entry>,
    bytes: Vec<u8>,
}
fn word(bytes: &[u8], at: usize) -> Result<u16, String> {
    let bytes = bytes.get(at..at + 2).ok_or("Truncated ZIP")?;
    Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
}
fn dword(bytes: &[u8], at: usize) -> Result<u32, String> {
    let bytes = bytes.get(at..at + 4).ok_or("Truncated ZIP")?;
    Ok(u32::from_le_bytes(bytes.try_into().unwrap()))
}
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
pub fn safe_name(name: &str) -> bool {
    !name.starts_with(['/', '\\'])
        && !name.contains('\0')
        && !name
            .replace('\\', "/")
            .split('/')
            .any(|part| part == ".." || part.contains(':'))
}
impl Archive {
    pub fn open(path: &Path, maximum: usize) -> Result<Self, String> {
        let file = std::fs::File::open(path).map_err(|error| error.to_string())?;
        let mut bytes = Vec::new();
        file.take(maximum.min(256 * 1024 * 1024) as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| error.to_string())?;
        if bytes.len() > maximum.min(256 * 1024 * 1024) {
            return Err("Archive exceeds the configured read-only browsing limit".into());
        }
        Self::parse(bytes)
    }
    pub fn parse(bytes: Vec<u8>) -> Result<Self, String> {
        let start = bytes.len().saturating_sub(65557);
        let end = bytes[start..]
            .windows(4)
            .rposition(|window| window == b"PK\x05\x06")
            .map(|offset| offset + start)
            .ok_or("ZIP end directory not found")?;
        if word(&bytes, end + 4)? != 0 || word(&bytes, end + 6)? != 0 {
            return Err("Split ZIP archives require an archive plugin".into());
        }
        let count = word(&bytes, end + 10)? as usize;
        if count > 4096 || count == 65535 {
            return Err("ZIP64 or more than 4096 members requires an archive plugin".into());
        }
        let size = dword(&bytes, end + 12)? as usize;
        let mut at = dword(&bytes, end + 16)? as usize;
        if at.checked_add(size).is_none_or(|limit| limit > end) {
            return Err("ZIP directory lies outside the archive".into());
        }
        let mut entries = Vec::new();
        for _ in 0..count {
            if bytes.get(at..at + 4) != Some(b"PK\x01\x02") {
                return Err("Invalid ZIP directory signature".into());
            }
            let flags = word(&bytes, at + 8)?;
            let method = word(&bytes, at + 10)?;
            let checksum = dword(&bytes, at + 16)?;
            let compressed = dword(&bytes, at + 20)?;
            let size = dword(&bytes, at + 24)?;
            let name_len = word(&bytes, at + 28)? as usize;
            let extra = word(&bytes, at + 30)? as usize;
            let comment = word(&bytes, at + 32)? as usize;
            let offset = dword(&bytes, at + 42)? as usize;
            let name_bytes = bytes
                .get(at + 46..at + 46 + name_len)
                .ok_or("Truncated ZIP member name")?
                .to_vec();
            let name = String::from_utf8_lossy(&name_bytes).into_owned();
            let safe = safe_name(&name);
            entries.push(Entry {
                name,
                compressed,
                size,
                method,
                flags,
                crc: checksum,
                offset,
                name_bytes,
                safe,
            });
            at = at
                .checked_add(46 + name_len + extra + comment)
                .ok_or("ZIP directory overflow")?;
        }
        Ok(Self { entries, bytes })
    }
    pub fn member(&self, index: usize) -> Result<Vec<u8>, String> {
        let entry = self.entries.get(index).ok_or("Select a ZIP member")?;
        if !entry.safe {
            return Err("Unsafe member name: preview refused".into());
        }
        if entry.name.ends_with('/') {
            return Ok(Vec::new());
        }
        if entry.flags & 1 != 0 {
            return Err("Encrypted ZIP member requires an archive plugin".into());
        }
        if entry.size > 8 * 1024 * 1024 || entry.compressed > 64 * 1024 * 1024 {
            return Err("Member exceeds the 8 MiB preview budget".into());
        }
        let at = entry.offset;
        if self.bytes.get(at..at + 4) != Some(b"PK\x03\x04") {
            return Err("Invalid ZIP local header".into());
        }
        if word(&self.bytes, at + 8)? != entry.method {
            return Err("ZIP compression metadata does not match".into());
        }
        let name_len = word(&self.bytes, at + 26)? as usize;
        let extra = word(&self.bytes, at + 28)? as usize;
        if self.bytes.get(at + 30..at + 30 + name_len) != Some(entry.name_bytes.as_slice()) {
            return Err("ZIP member name does not match its directory".into());
        }
        let start = at
            .checked_add(30 + name_len + extra)
            .ok_or("ZIP offset overflow")?;
        let data = self
            .bytes
            .get(
                start
                    ..start
                        .checked_add(entry.compressed as usize)
                        .ok_or("ZIP size overflow")?,
            )
            .ok_or("Truncated ZIP member")?;
        let mut result = Vec::new();
        match entry.method {
            0 => result.extend_from_slice(data),
            8 => {
                flate2::read::DeflateDecoder::new(data)
                    .take(8 * 1024 * 1024 + 1)
                    .read_to_end(&mut result)
                    .map_err(|error| error.to_string())?;
            }
            _ => return Err(
                "Compression method requires an archive plugin (stored and deflate are supported)"
                    .into(),
            ),
        }
        if result.len() != entry.size as usize || crc(&result) != entry.crc {
            return Err("ZIP member size/checksum mismatch".into());
        }
        Ok(result)
    }
}
#[cfg(test)]
mod fixtures {
    use super::*;
    #[test]
    fn traversal_and_windows_names_are_rejected() {
        for name in [
            "../secrets",
            "a/../../b",
            "/absolute",
            "C:\\file",
            "a\\..\\file",
        ] {
            assert!(!safe_name(name));
        }
        assert!(safe_name("package/π.json"));
    }
    #[test]
    fn malformed_archive_cannot_address_outside_its_buffer() {
        assert!(Archive::parse(b"PK\x05\x06".to_vec()).is_err());
    }
}

#[cfg(test)]
mod member_fixtures {
    use super::*;
    fn stored() -> Vec<u8> {
        let name = b"data.json";
        let data = b"{\"value\":42}";
        let sum = crc(data);
        let mut bytes = Vec::new();
        let w = |bytes: &mut Vec<u8>, value: u16| bytes.extend(value.to_le_bytes());
        let d = |bytes: &mut Vec<u8>, value: u32| bytes.extend(value.to_le_bytes());
        bytes.extend(b"PK\x03\x04");
        for value in [20, 0, 0, 0, 0] {
            w(&mut bytes, value);
        }
        d(&mut bytes, sum);
        d(&mut bytes, data.len() as u32);
        d(&mut bytes, data.len() as u32);
        w(&mut bytes, name.len() as u16);
        w(&mut bytes, 0);
        bytes.extend(name);
        bytes.extend(data);
        let offset = bytes.len();
        bytes.extend(b"PK\x01\x02");
        for value in [20, 20, 0, 0, 0, 0] {
            w(&mut bytes, value);
        }
        d(&mut bytes, sum);
        d(&mut bytes, data.len() as u32);
        d(&mut bytes, data.len() as u32);
        for value in [name.len() as u16, 0, 0, 0, 0] {
            w(&mut bytes, value);
        }
        d(&mut bytes, 0);
        d(&mut bytes, 0);
        bytes.extend(name);
        let size = bytes.len() - offset;
        bytes.extend(b"PK\x05\x06");
        for value in [0, 0, 1, 1] {
            w(&mut bytes, value);
        }
        d(&mut bytes, size as u32);
        d(&mut bytes, offset as u32);
        w(&mut bytes, 0);
        bytes
    }
    #[test]
    fn stored_member_is_bounded_and_checked_before_preview() {
        let bytes = stored();
        let archive = Archive::parse(bytes.clone()).unwrap();
        assert_eq!(archive.entries[0].name, "data.json");
        assert_eq!(archive.member(0).unwrap(), b"{\"value\":42}");
        let mut corrupt = bytes;
        corrupt[30 + b"data.json".len()] ^= 1;
        assert!(Archive::parse(corrupt).unwrap().member(0).is_err());
    }
}

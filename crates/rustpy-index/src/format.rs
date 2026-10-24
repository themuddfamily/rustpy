//! The index file format.
//!
//! ```text
//! magic            8 bytes  "RPYIDX\r\n"
//! format version   u32 LE   layout of this file (FORMAT_VERSION)
//! schema version   u32 LE   what extraction records (SCHEMA_VERSION)
//! meta length      u32 LE
//! meta             JSON     IndexMeta (kind, label, owner, counts, times)
//! body             zlib     everything else, see `encode_body`
//! ```
//!
//! The header is readable without decompressing the body, so listing indexes
//! is cheap. A file with another format or schema version is not loaded: the
//! index is rebuilt (bump [`SCHEMA_VERSION`] whenever [`crate::extract`]
//! changes what it records). The zlib stream carries an Adler-32 checksum, and
//! every count and string id is checked while decoding, so a damaged file is
//! rejected instead of producing garbage.
//!
//! Bundles are imported from anywhere, so decoding also bounds memory by the
//! size of the file: the body may expand at most [`MAX_RATIO`] times the
//! compressed bytes it was read from (nothing may follow the zlib stream), and
//! every record in the body costs a few bytes there, so the decoded tables
//! take at most a small multiple of the body.

use std::io::{Read, Write};
use std::path::PathBuf;

use crate::extract::{SymbolKind, STORED_FLAGS};
use crate::hash::ContentHash;
use crate::index::{
    FileRec, IndexMeta, Root, RootKind, Strings, SymRec, SymbolIndex, FILE_STORED_FLAGS, NONE,
};

pub const MAGIC: [u8; 8] = *b"RPYIDX\r\n";
/// Layout of index files.
/// 2: every symbol records its class bases (a string id after `origin`).
pub const FORMAT_VERSION: u32 = 2;
/// What symbol extraction records; part of the header so that indexes built
/// by an older extractor are rebuilt.
/// 2: every star import of a module is recorded (1 kept only the first).
/// 3: receiver-assigned instance attributes are indexed as class members.
/// 4: classes record their declared bases (hierarchy candidates).
pub const SCHEMA_VERSION: u32 = 4;
const END: [u8; 4] = *b"END\0";
/// Largest header metadata accepted.
const MAX_META: u32 = 1 << 20;
/// Largest decompressed body accepted. A large environment (half a million
/// symbols) takes about 15 MB; much more is a damaged or hostile file
/// (bundles are imported from anywhere), and is refused before it fills memory.
const MAX_BODY: u64 = 256 << 20;
/// Largest decompressed/compressed size ratio accepted. Index bodies compress
/// about 2× (`real_env.rs` reports it); zlib allows ~1000×. Decoding takes a
/// small multiple of the body (`tests/hostile_bundles.rs` measures it), so a
/// file cannot make the decoder allocate more than about 100× its size.
pub const MAX_RATIO: u64 = 8;
/// Bytes a body may always expand to, whatever its compressed size.
const RATIO_SLACK: u64 = 4 << 20;
/// Largest index file read. A body within [`MAX_BODY`] never compresses to
/// more (zlib adds five bytes per 64 KiB stored block), so a larger file is
/// refused before it is read into memory.
pub const MAX_FILE: u64 = 20 + MAX_META as u64 + MAX_BODY + MAX_BODY / 8192 + 64;

/// Why a file could not be read as an index.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FormatError {
    NotAnIndex,
    /// Written by another version of RustPy: `(format, schema)` found.
    Version(u32, u32),
    Damaged(String),
}

impl std::fmt::Display for FormatError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FormatError::NotAnIndex => write!(f, "not a RustPy index file"),
            FormatError::Version(fv, sv) => write!(f, "written by another RustPy version (format {fv}, schema {sv}; this version reads {FORMAT_VERSION}/{SCHEMA_VERSION})"),
            FormatError::Damaged(m) => write!(f, "damaged index file ({m})"),
        }
    }
}

impl std::error::Error for FormatError {}

fn damaged(m: impl Into<String>) -> FormatError {
    FormatError::Damaged(m.into())
}

/// Serializes `index` (header and compressed body).
pub fn encode(index: &SymbolIndex) -> Vec<u8> {
    let meta = serde_json::to_vec(&index.meta).unwrap_or_else(|_| b"{}".to_vec());
    let mut out = Vec::with_capacity(64 + meta.len());
    out.extend_from_slice(&MAGIC);
    out.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
    out.extend_from_slice(&SCHEMA_VERSION.to_le_bytes());
    out.extend_from_slice(&(meta.len() as u32).to_le_bytes());
    out.extend_from_slice(&meta);
    let body = encode_body(index);
    let mut z = flate2::write::ZlibEncoder::new(out, flate2::Compression::fast());
    // Writing into a Vec cannot fail.
    let _ = z.write_all(&body);
    z.finish().unwrap_or_default()
}

/// Reads only the header of an index file: its metadata.
pub fn read_meta(mut r: impl Read) -> Result<IndexMeta, FormatError> {
    let mut head = [0u8; 20];
    r.read_exact(&mut head)
        .map_err(|_| FormatError::NotAnIndex)?;
    let meta_len = check_header(&head)?;
    let mut meta = vec![0u8; meta_len as usize];
    r.read_exact(&mut meta)
        .map_err(|_| damaged("truncated header"))?;
    serde_json::from_slice(&meta).map_err(|e| damaged(format!("header: {e}")))
}

fn check_header(head: &[u8; 20]) -> Result<u32, FormatError> {
    if head[..8] != MAGIC {
        return Err(FormatError::NotAnIndex);
    }
    let format = u32::from_le_bytes(head[8..12].try_into().unwrap_or_default());
    let schema = u32::from_le_bytes(head[12..16].try_into().unwrap_or_default());
    if format != FORMAT_VERSION || schema != SCHEMA_VERSION {
        return Err(FormatError::Version(format, schema));
    }
    let meta_len = u32::from_le_bytes(head[16..20].try_into().unwrap_or_default());
    if meta_len > MAX_META {
        return Err(damaged("header too large"));
    }
    Ok(meta_len)
}

/// Deserializes an index written by [`encode`].
pub fn decode(bytes: &[u8]) -> Result<SymbolIndex, FormatError> {
    let head: &[u8; 20] = bytes
        .get(..20)
        .and_then(|h| h.try_into().ok())
        .ok_or(FormatError::NotAnIndex)?;
    let meta_len = check_header(head)? as usize;
    let meta_bytes = bytes
        .get(20..20 + meta_len)
        .ok_or_else(|| damaged("truncated header"))?;
    let meta: IndexMeta =
        serde_json::from_slice(meta_bytes).map_err(|e| damaged(format!("header: {e}")))?;
    let body = inflate(&bytes[20 + meta_len..])?;
    decode_body(meta, &body)
}

/// Decompresses a body. It is refused as soon as it expands more than
/// [`MAX_RATIO`] times the compressed bytes consumed so far (padding after
/// the stream cannot raise the limit: it is refused too), so a small file
/// never makes the decoder allocate much.
fn inflate(compressed: &[u8]) -> Result<Vec<u8>, FormatError> {
    let mut z = flate2::read::ZlibDecoder::new(compressed);
    let mut body = Vec::new();
    let mut chunk = vec![0u8; 64 << 10];
    loop {
        let n = z
            .read(&mut chunk)
            .map_err(|e| damaged(format!("body: {e}")))?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..n]);
        let limit = MAX_BODY.min(
            z.total_in()
                .saturating_mul(MAX_RATIO)
                .saturating_add(RATIO_SLACK),
        );
        if body.len() as u64 > limit {
            return Err(damaged("body too large"));
        }
    }
    if z.total_in() != compressed.len() as u64 {
        return Err(damaged("data after the body"));
    }
    Ok(body)
}

// ----- body ------------------------------------------------------------------
//
// strings   count, then (length, UTF-8 bytes) each; id = position
// roots     count, then (path string id, kind) each
// files     count, then per file:
//             root, rel path id, module id, size, mtime (zigzag), hash (16 bytes LE), flags,
//             `__all__` count + string ids,
//             symbol count, then per symbol:
//               name id, kind, flags, line, parent (index in the file + 1, 0 = none), detail id, origin id, bases id
// "END\0"
//
// Integers are unsigned LEB128 varints unless noted.

fn encode_body(index: &SymbolIndex) -> Vec<u8> {
    let mut w = Writer {
        out: Vec::with_capacity(index.strings.len() * 12 + index.symbols.len() * 8),
    };
    w.uint(index.strings.len() as u64);
    for i in 0..index.strings.len() {
        w.string(index.strings.get(i as u32));
    }
    // Root paths are not in the string table.
    w.uint(index.roots.len() as u64);
    for r in &index.roots {
        w.string(&r.path.to_string_lossy());
        w.byte(r.kind.code());
    }
    w.uint(index.files.len() as u64);
    for f in &index.files {
        w.uint(f.root as u64);
        w.uint(f.rel as u64);
        w.uint(f.module as u64);
        w.uint(f.size);
        w.uint(zigzag(f.mtime_ns));
        w.out.extend_from_slice(&f.hash.0.to_le_bytes());
        w.byte(f.flags & FILE_STORED_FLAGS);
        w.uint(f.all_len as u64);
        for &n in &index.all_names[f.all_start as usize..(f.all_start + f.all_len) as usize] {
            w.uint(n as u64);
        }
        w.uint(f.sym_len as u64);
        for s in &index.symbols[f.sym_start as usize..(f.sym_start + f.sym_len) as usize] {
            w.uint(s.name as u64);
            w.byte(s.kind.code());
            w.byte(s.flags & STORED_FLAGS);
            w.uint(s.line as u64);
            w.uint(if s.parent == NONE {
                0
            } else {
                (s.parent - f.sym_start + 1) as u64
            });
            w.uint(s.detail as u64);
            w.uint(s.origin as u64);
            w.uint(s.bases as u64);
        }
    }
    w.out.extend_from_slice(&END);
    w.out
}

fn decode_body(meta: IndexMeta, body: &[u8]) -> Result<SymbolIndex, FormatError> {
    let mut r = Reader { data: body, pos: 0 };
    // Strings are interned: only the first one (id 0) is empty, every other
    // one takes at least two bytes.
    let string_count = r.count(2, 1)?;
    let mut strings = Strings::default();
    strings.reserve(string_count, body.len().min(1 << 28));
    for i in 0..string_count {
        let s = r.str()?;
        if s.is_empty() && i > 0 {
            return Err(damaged("empty string"));
        }
        strings.push(s);
    }
    let sid = |v: u64| -> Result<u32, FormatError> {
        if (v as usize) < string_count {
            Ok(v as u32)
        } else {
            Err(damaged("bad string id"))
        }
    };
    let root_count = r.count(2, 0)?;
    let mut roots = Vec::with_capacity(root_count);
    for _ in 0..root_count {
        let path = PathBuf::from(r.str()?);
        let kind = RootKind::from_code(r.byte()?).ok_or_else(|| damaged("bad root kind"))?;
        roots.push(Root { path, kind });
    }
    let file_count = r.count(24, 0)?;
    let mut files = Vec::with_capacity(file_count);
    let mut all_names = Vec::new();
    let mut symbols: Vec<SymRec> = Vec::new();
    for fi in 0..file_count {
        let root = r.uint()?;
        if root as usize >= root_count {
            return Err(damaged("bad root"));
        }
        let rel = sid(r.uint()?)?;
        let module = sid(r.uint()?)?;
        let size = r.uint()?;
        let mtime_ns = unzigzag(r.uint()?);
        let hash = ContentHash(u128::from_le_bytes(
            r.bytes(16)?.try_into().map_err(|_| damaged("hash"))?,
        ));
        let flags = r.byte()? & FILE_STORED_FLAGS;
        let all_start = all_names.len() as u32;
        let all_len = r.count(1, 0)?;
        for _ in 0..all_len {
            all_names.push(sid(r.uint()?)?);
        }
        let sym_start = symbols.len() as u32;
        let sym_len = r.count(8, 0)?;
        for _ in 0..sym_len {
            let name = sid(r.uint()?)?;
            let kind =
                SymbolKind::from_code(r.byte()?).ok_or_else(|| damaged("bad symbol kind"))?;
            let sflags = r.byte()? & STORED_FLAGS;
            let line = u32::try_from(r.uint()?).map_err(|_| damaged("bad line"))?;
            let parent = match r.uint()? {
                0 => NONE,
                p if (p as usize) <= sym_len => sym_start + p as u32 - 1,
                _ => return Err(damaged("bad parent")),
            };
            let detail = sid(r.uint()?)?;
            let origin = sid(r.uint()?)?;
            let bases = sid(r.uint()?)?;
            symbols.push(SymRec {
                name,
                detail,
                origin,
                file: fi as u32,
                line,
                parent,
                kind,
                flags: sflags,
                bases,
            });
        }
        files.push(FileRec {
            root: root as u32,
            rel,
            module,
            size,
            mtime_ns,
            hash,
            flags,
            all_start,
            all_len: all_len as u32,
            sym_start,
            sym_len: sym_len as u32,
        });
    }
    if r.bytes(4)? != END || r.pos != body.len() {
        return Err(damaged("trailing data"));
    }
    Ok(SymbolIndex::from_parts(
        meta, strings, roots, files, all_names, symbols,
    ))
}

fn zigzag(v: i64) -> u64 {
    ((v << 1) ^ (v >> 63)) as u64
}

fn unzigzag(v: u64) -> i64 {
    ((v >> 1) as i64) ^ -((v & 1) as i64)
}

struct Writer {
    out: Vec<u8>,
}

impl Writer {
    fn uint(&mut self, mut v: u64) {
        loop {
            let b = (v & 0x7f) as u8;
            v >>= 7;
            if v == 0 {
                self.out.push(b);
                return;
            }
            self.out.push(b | 0x80);
        }
    }

    fn byte(&mut self, b: u8) {
        self.out.push(b);
    }

    fn string(&mut self, s: &str) {
        self.uint(s.len() as u64);
        self.out.extend_from_slice(s.as_bytes());
    }
}

struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn byte(&mut self) -> Result<u8, FormatError> {
        let b = *self
            .data
            .get(self.pos)
            .ok_or_else(|| damaged("truncated"))?;
        self.pos += 1;
        Ok(b)
    }

    fn uint(&mut self) -> Result<u64, FormatError> {
        let mut v = 0u64;
        for shift in (0..64).step_by(7) {
            let b = self.byte()?;
            if shift == 63 && b > 1 {
                return Err(damaged("bad number"));
            }
            v |= u64::from(b & 0x7f) << shift;
            if b & 0x80 == 0 {
                return Ok(v);
            }
        }
        Err(damaged("bad number"))
    }

    /// A count of items that take at least `min_bytes` each (`short` of
    /// them one byte less): larger than what is left means a damaged file
    /// (and would allocate absurd amounts).
    fn count(&mut self, min_bytes: usize, short: u64) -> Result<usize, FormatError> {
        let n = self.uint()?;
        let left = (self.data.len() - self.pos) as u64;
        if n.saturating_mul(min_bytes as u64) > left.saturating_add(short.min(n)) {
            return Err(damaged("count exceeds data"));
        }
        Ok(n as usize)
    }

    fn bytes(&mut self, n: usize) -> Result<&'a [u8], FormatError> {
        let end = self
            .pos
            .checked_add(n)
            .filter(|e| *e <= self.data.len())
            .ok_or_else(|| damaged("truncated"))?;
        let s = &self.data[self.pos..end];
        self.pos = end;
        Ok(s)
    }

    fn str(&mut self) -> Result<&'a str, FormatError> {
        let n = self.count(1, 0)?;
        std::str::from_utf8(self.bytes(n)?).map_err(|_| damaged("invalid UTF-8"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::tests::index_of;

    fn sample() -> SymbolIndex {
        let mut idx = index_of(
            &[RootKind::Stdlib, RootKind::Site],
            &[
                (0, "json/__init__.py", "from .decoder import JSONDecoder\n__all__ = ['JSONDecoder', 'loads']\ndef loads(s, *, cls=None) -> object:\n    pass\n"),
                (0, "json/decoder.py", "class JSONDecoder(object):\n    def decode(self, s):\n        pass\n    class Nested:\n        X = 1\n"),
                (1, "pkg/__init__.py", "from ._core import *\nimport numpy as np\n"),
                (1, "pkg/_core.py", "def größe(): pass\nπ = 3\n"),
                (1, "empty.py", ""),
            ],
        );
        idx.set_meta(IndexMeta {
            label: "Python 3.12 (venv)".into(),
            owner: "/usr/bin/python3".into(),
            python_version: "3.12.3".into(),
            created: 1,
            updated: 2,
            ..Default::default()
        });
        idx
    }

    #[test]
    fn round_trip() {
        let idx = sample();
        let bytes = encode(&idx);
        let back = decode(&bytes).unwrap();
        assert!(back.same_content(&idx));
        assert_eq!(back.meta(), idx.meta());
        assert_eq!(back.meta().files, 5);
        assert_eq!(read_meta(&bytes[..]).unwrap(), *idx.meta());
        // Derived tables are rebuilt: queries give the same answers.
        let score = crate::index::tests::score;
        let opts = crate::index::SearchOptions::default();
        assert_eq!(
            back.search("dec", &opts, &score),
            idx.search("dec", &opts, &score)
        );
        assert_eq!(back.search("größe", &opts, &score).len(), 1);
        let m = |n: &str| n.contains('l');
        assert_eq!(
            back.import_candidates("l", &m, 10),
            idx.import_candidates("l", &m, 10)
        );
        // An empty index too.
        let empty = SymbolIndex::empty(IndexMeta::default());
        assert!(decode(&encode(&empty)).unwrap().same_content(&empty));
    }

    #[test]
    fn class_bases_round_trip() {
        let idx = crate::index::tests::index_of(
            &[RootKind::Project],
            &[
                (0, "base.py", "class Base: pass\n"),
                (0, "child.py", "from base import Base as Parent\nimport base\nAlias = Parent\nclass Child(Parent, base.Mixin[int]):\n    class Inner(Child): pass\n"),
            ],
        );
        let back = decode(&encode(&idx)).unwrap();
        assert!(back.same_content(&idx));
        let bases = back.class_bases();
        assert_eq!(bases, idx.class_bases());
        let child = bases.iter().find(|f| f.path.ends_with("child.py")).unwrap();
        assert_eq!(
            child
                .classes
                .iter()
                .map(|c| (c.name.as_str(), c.bases.clone()))
                .collect::<Vec<_>>(),
            [
                (
                    "Child",
                    vec!["Parent".to_string(), "base.Mixin".to_string()]
                ),
                ("Inner", vec!["Child".to_string()]),
            ]
        );
        assert_eq!(child.imports, [("Parent".to_string(), "Base".to_string())]);
        assert_eq!(child.variables, ["Alias"]);
        assert!(!child.hash.is_none());
        // Fresh extraction gives the same record as the index.
        let source = "from base import Base as Parent\nimport base\nAlias = Parent\nclass Child(Parent, base.Mixin[int]):\n    class Inner(Child): pass\n";
        assert_eq!(
            crate::index::FileBases::from_symbols(
                child.path.clone(),
                child.hash,
                &crate::extract::extract(source)
            ),
            *child
        );
    }

    #[test]
    fn rejects_other_versions_and_damage() {
        let bytes = encode(&sample());
        assert_eq!(decode(b"").err(), Some(FormatError::NotAnIndex));
        assert_eq!(
            decode(b"PK\x03\x04 a zip file, not an index").err(),
            Some(FormatError::NotAnIndex)
        );
        let mut other = bytes.clone();
        other[12..16].copy_from_slice(&(SCHEMA_VERSION + 1).to_le_bytes());
        assert_eq!(
            decode(&other).err(),
            Some(FormatError::Version(FORMAT_VERSION, SCHEMA_VERSION + 1))
        );
        assert!(matches!(
            read_meta(&other[..]),
            Err(FormatError::Version(..))
        ));
        // Truncated anywhere, or any byte of the body flipped: an error, never a panic.
        for cut in [21, bytes.len() / 2, bytes.len() - 1] {
            assert!(decode(&bytes[..cut]).is_err(), "cut at {cut}");
        }
        let body_start = 20 + u32::from_le_bytes(bytes[16..20].try_into().unwrap()) as usize;
        for i in body_start..bytes.len() {
            let mut b = bytes.clone();
            b[i] ^= 0x55;
            let _ = decode(&b);
        }
        // A tiny file that decompresses to far more than an index would (a zip bomb).
        let mut bomb = bytes[..body_start].to_vec();
        let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::best());
        z.write_all(&vec![0u8; 8 << 20]).unwrap();
        bomb.extend(z.finish().unwrap());
        assert!(bomb.len() < 64 << 10);
        assert_eq!(
            decode(&bomb).err(),
            Some(FormatError::Damaged("body too large".into()))
        );
    }

    #[test]
    fn padding_cannot_raise_the_size_limit() {
        let bytes = encode(&sample());
        let mut padded = bytes.clone();
        padded.extend(std::iter::repeat_n(0u8, 4096));
        assert_eq!(
            decode(&padded).err(),
            Some(FormatError::Damaged("data after the body".into()))
        );
        // The reviewed attack: 64 MB of empty strings compressed to ~64 KB and
        // padded to 1 MB (the limit came from the padded size): refused after
        // a few MB of body, not after allocating gigabytes.
        let body_start = 20 + u32::from_le_bytes(bytes[16..20].try_into().unwrap()) as usize;
        let mut w = Writer { out: Vec::new() };
        w.uint(64 << 20);
        let mut body = w.out;
        body.resize(body.len() + (64 << 20), 0);
        let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::best());
        z.write_all(&body).unwrap();
        let mut hostile = bytes[..body_start].to_vec();
        hostile.extend(z.finish().unwrap());
        assert!(hostile.len() < 1 << 20);
        hostile.resize(1 << 20, 0);
        let started = std::time::Instant::now();
        assert_eq!(
            decode(&hostile).err(),
            Some(FormatError::Damaged("body too large".into()))
        );
        assert!(
            started.elapsed() < std::time::Duration::from_secs(2),
            "{:?}",
            started.elapsed()
        );
    }

    #[test]
    fn overflowing_varints_are_rejected() {
        let mut bytes = vec![0xff; 9];
        bytes.push(1);
        assert_eq!(
            Reader {
                data: &bytes,
                pos: 0
            }
            .uint()
            .unwrap(),
            u64::MAX
        );
        bytes[9] = 2;
        assert_eq!(
            Reader {
                data: &bytes,
                pos: 0
            }
            .uint()
            .err(),
            Some(damaged("bad number"))
        );
        bytes[9] = 0x81;
        assert_eq!(
            Reader {
                data: &bytes,
                pos: 0
            }
            .uint()
            .err(),
            Some(damaged("bad number"))
        );
    }

    #[test]
    fn only_the_first_string_is_empty() {
        let meta = IndexMeta::default();
        let body = |strings: &[&str]| {
            let mut w = Writer { out: Vec::new() };
            w.uint(strings.len() as u64);
            for s in strings {
                w.string(s);
            }
            w.uint(0); // roots
            w.uint(0); // files
            w.out.extend_from_slice(&END);
            w.out
        };
        assert!(decode_body(meta.clone(), &body(&["", "a", "bc"])).is_ok());
        assert_eq!(
            decode_body(meta.clone(), &body(&["", "a", ""])).err(),
            Some(FormatError::Damaged("empty string".into()))
        );
        // A count of strings that cannot take two bytes each is refused before anything is allocated.
        let mut w = Writer { out: Vec::new() };
        w.uint(1000);
        w.out.extend(std::iter::repeat_n(0u8, 1500));
        assert_eq!(
            decode_body(meta, &w.out).err(),
            Some(FormatError::Damaged("count exceeds data".into()))
        );
    }

    #[test]
    fn damaged_bodies_are_rejected_not_trusted() {
        // Hand-made bodies with bad ids and counts (valid zlib, valid header).
        let meta = IndexMeta::default();
        let check = |body: &[u8]| decode_body(meta.clone(), body).err();
        assert!(
            matches!(
                check(&[0xff, 0xff, 0xff, 0xff, 0x0f]),
                Some(FormatError::Damaged(_))
            ),
            "a huge string count"
        );
        // One string, no roots, one file pointing at root 0.
        assert!(matches!(
            check(&[1, 0, 0, 1, 0]),
            Some(FormatError::Damaged(_))
        ));
        // One string, one root, one file with a rel id out of range.
        let mut w = Writer { out: Vec::new() };
        w.uint(1);
        w.string("");
        w.uint(1);
        w.string("/r");
        w.byte(0);
        w.uint(1);
        w.uint(0);
        w.uint(7);
        assert!(matches!(check(&w.out), Some(FormatError::Damaged(_))));
        assert_eq!(unzigzag(zigzag(-5)), -5);
        assert_eq!(unzigzag(zigzag(i64::MAX)), i64::MAX);
        assert_eq!(unzigzag(zigzag(i64::MIN)), i64::MIN);
    }
}

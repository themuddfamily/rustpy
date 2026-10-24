//! Hostile bundles (shared indexes are imported from anywhere): however a
//! file is crafted, loading it allocates at most a small multiple of its size.
//! Measured with a counting allocator, so this binary holds a single test.

use std::alloc::{GlobalAlloc, Layout, System};
use std::io::Write;
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};

use rustpy_index::format::{FORMAT_VERSION, MAGIC, MAX_FILE, MAX_RATIO, SCHEMA_VERSION};
use rustpy_index::IndexStore;

struct Counting;

static CURRENT: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

fn grew(by: usize) {
    let now = CURRENT.fetch_add(by, Relaxed) + by;
    PEAK.fetch_max(now, Relaxed);
}

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let p = unsafe { System.alloc(layout) };
        if !p.is_null() {
            grew(layout.size());
        }
        p
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let p = unsafe { System.alloc_zeroed(layout) };
        if !p.is_null() {
            grew(layout.size());
        }
        p
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) };
        CURRENT.fetch_sub(layout.size(), Relaxed);
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let p = unsafe { System.realloc(ptr, layout, new_size) };
        if !p.is_null() {
            if new_size >= layout.size() {
                grew(new_size - layout.size());
            } else {
                CURRENT.fetch_sub(layout.size() - new_size, Relaxed);
            }
        }
        p
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// Bytes allocated at most while `f` runs, above what was allocated before.
fn peak_during<T>(f: impl FnOnce() -> T) -> (T, usize) {
    let base = CURRENT.load(Relaxed);
    PEAK.store(base, Relaxed);
    let out = f();
    (out, PEAK.load(Relaxed).saturating_sub(base))
}

fn varint(out: &mut Vec<u8>, mut v: u64) {
    loop {
        let b = (v & 0x7f) as u8;
        v >>= 7;
        if v == 0 {
            out.push(b);
            return;
        }
        out.push(b | 0x80);
    }
}

/// An index file around `body` (compressed), padded with zeros to `pad_to` bytes.
fn index_file(body: &[u8], pad_to: usize) -> Vec<u8> {
    let meta = b"{\"kind\":\"bundle\",\"label\":\"hostile\"}";
    let mut out = Vec::new();
    out.extend_from_slice(&MAGIC);
    out.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
    out.extend_from_slice(&SCHEMA_VERSION.to_le_bytes());
    out.extend_from_slice(&(meta.len() as u32).to_le_bytes());
    out.extend_from_slice(meta);
    let mut z = flate2::write::ZlibEncoder::new(out, flate2::Compression::fast());
    z.write_all(body).unwrap();
    let mut out = z.finish().unwrap();
    if out.len() < pad_to {
        out.resize(pad_to, 0);
    }
    out
}

/// A body built for the most memory per byte that still decodes: `n`
/// one-letter strings and `n` module-level symbols, each named by its own
/// string (one name row each). With `varied` lines and details it
/// compresses like a real index, otherwise far better.
fn dense_body(n: u64, varied: bool) -> Vec<u8> {
    let mut b = Vec::new();
    varint(&mut b, n + 1);
    varint(&mut b, 0);
    for i in 0..n {
        varint(&mut b, 1);
        b.push(b'a' + (i % 26) as u8);
    }
    // One root, one file.
    varint(&mut b, 1);
    varint(&mut b, 2);
    b.extend_from_slice(b"/r");
    b.push(2);
    varint(&mut b, 1);
    varint(&mut b, 0); // root
    varint(&mut b, 1); // rel
    varint(&mut b, 2); // module
    varint(&mut b, 100); // size
    varint(&mut b, 0); // mtime
    b.extend_from_slice(&[7u8; 16]); // hash
    b.push(0); // flags
    varint(&mut b, 0); // __all__
    varint(&mut b, n);
    let mut state = 0x9e37_79b9_7f4a_7c15u64;
    for i in 0..n {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        let (line, detail) = if varied {
            (state % 100_000, (state >> 20) % (n + 1))
        } else {
            (0, 0)
        };
        varint(&mut b, i + 1); // name
        b.push(1); // function
        b.push(0); // flags
        varint(&mut b, line);
        varint(&mut b, 0); // parent
        varint(&mut b, detail);
        varint(&mut b, 0); // origin
        varint(&mut b, 0); // bases
    }
    b.extend_from_slice(b"END\0");
    b
}

#[test]
fn hostile_bundles_cannot_take_much_more_memory_than_their_size() {
    let dir = tempfile::tempdir().unwrap();
    let store = IndexStore::new(dir.path().join("index"));

    // The reviewed attack: 64 MB of empty strings (~64 KB compressed) padded
    // to 1 MB. Before: 1.19 GB and 23 s before the import was refused.
    let mut body = Vec::new();
    varint(&mut body, 64 << 20);
    body.resize(body.len() + (64 << 20), 0);
    let bomb = index_file(&body, 0);
    drop(body);
    let mut padded = bomb.clone();
    padded.resize(1 << 20, 0);
    let path = dir.path().join("padded.rpidx");
    std::fs::write(&path, &padded).unwrap();
    let started = std::time::Instant::now();
    let (result, peak) = peak_during(|| store.import_bundle(&path).map(|_| ()));
    assert!(result.is_err());
    assert!(peak < 16 << 20, "peak {} MB", peak >> 20);
    assert!(
        started.elapsed() < std::time::Duration::from_secs(5),
        "{:?}",
        started.elapsed()
    );
    // The same without padding.
    let path = dir.path().join("bomb.rpidx");
    std::fs::write(&path, &bomb).unwrap();
    let (result, peak) = peak_during(|| IndexStore::load(&path).map(|_| ()));
    assert!(result.is_err());
    assert!(peak < 16 << 20, "peak {} MB", peak >> 20);

    // A file larger than any index (sparse: it takes no disk space) is
    // refused before it is read; so is something that is not a file.
    let huge = dir.path().join("huge.rpidx");
    std::fs::File::create(&huge)
        .unwrap()
        .set_len(MAX_FILE + 1)
        .unwrap();
    let (result, peak) = peak_during(|| store.import_bundle(&huge).map(|_| ()));
    assert!(result.unwrap_err().to_string().contains("file too large"));
    assert!(peak < 1 << 20, "peak {} KB", peak >> 10);
    let (result, peak) = peak_during(|| IndexStore::load(dir.path()).map(|_| ()));
    assert!(result.is_err() && peak < 1 << 20);
    #[cfg(unix)]
    {
        // Endless, and reported as 0 bytes long.
        let (result, peak) =
            peak_during(|| IndexStore::load(std::path::Path::new("/dev/zero")).map(|_| ()));
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("not a RustPy index"));
        assert!(peak < 1 << 20, "peak {} KB", peak >> 10);
    }

    // Bodies that do decode: memory stays within a small multiple of the
    // body, and the body within MAX_RATIO of the file.
    for (n, varied) in [(200_000u64, true), (600_000, true), (600_000, false)] {
        let body = dense_body(n, varied);
        let file = index_file(&body, 0);
        let ratio = body.len() as f64 / file.len() as f64;
        let path = dir.path().join(format!("dense-{n}.rpidx"));
        std::fs::write(&path, &file).unwrap();
        let (result, peak) = peak_during(|| IndexStore::load(&path).map(|idx| idx.symbol_count()));
        eprintln!("{n} symbols: file {} KB, body {} KB (ratio {ratio:.1}), result {result:?}, peak {} MB = {:.1}x the body, {:.1}x the file", file.len() >> 10, body.len() >> 10, peak >> 20, peak as f64 / body.len() as f64, peak as f64 / file.len() as f64);
        if ratio <= MAX_RATIO as f64 {
            assert_eq!(result.unwrap(), n as usize);
        }
        // About 8–12× in practice (vectors grow by doubling).
        assert!(
            peak as f64 <= 16.0 * body.len() as f64,
            "peak {} MB for a {} KB body",
            peak >> 20,
            body.len() >> 10
        );
        assert!(
            (peak as u64) <= 16 * (MAX_RATIO * file.len() as u64 + (4 << 20)),
            "peak {} MB for a {} KB file",
            peak >> 20,
            file.len() >> 10
        );
    }
}

//! Indexes the `python3` found on PATH (its `sys.path` directories) and prints
//! timings and sizes. Ignored by default (machine dependent, slow):
//!
//! ```sh
//! cargo test -p rustpy-index --release --test real_env -- --ignored --nocapture
//! ```

use std::path::PathBuf;
use std::time::Instant;

use rustpy_index::{
    refresh, walk_library_roots, BuildOptions, IndexMeta, IndexStore, Progress, RootKind, RootSpec,
    SearchOptions,
};

fn interpreter_paths() -> Option<Vec<PathBuf>> {
    let out = std::process::Command::new("python3")
        .args([
            "-I",
            "-c",
            "import sys, json; print(json.dumps([p for p in sys.path if p]))",
        ])
        .output()
        .ok()?;
    let paths: Vec<String> = serde_json::from_slice(&out.stdout).ok()?;
    Some(
        paths
            .into_iter()
            .map(PathBuf::from)
            .filter(|p| p.is_dir())
            .collect(),
    )
}

fn score(name: &str, q: &str) -> Option<i32> {
    let (n, q) = (name.to_lowercase(), q.to_lowercase());
    if n.starts_with(&q) {
        Some(100)
    } else {
        n.contains(&q).then_some(50)
    }
}

#[test]
#[ignore]
fn index_the_local_interpreter() {
    let Some(paths) = interpreter_paths() else {
        eprintln!("no python3");
        return;
    };
    let roots: Vec<RootSpec> = paths
        .iter()
        .map(|p| RootSpec {
            path: p.clone(),
            kind: RootKind::classify(p),
        })
        .collect();
    eprintln!("roots: {roots:?}");
    let progress = Progress::default();
    let t = Instant::now();
    let files = walk_library_roots(&roots, &progress);
    eprintln!("walk: {} files in {:?}", files.len(), t.elapsed());
    let opts = BuildOptions::default();
    let t = Instant::now();
    let (index, stats) = refresh(
        None,
        IndexMeta {
            label: "python3".into(),
            ..Default::default()
        },
        &roots,
        files.clone(),
        &[],
        &opts,
        &progress,
    )
    .unwrap();
    let index = index.expect("a new index");
    eprintln!(
        "build ({} threads): {} in {:?}; {} symbols, {} modules",
        opts.threads,
        stats.summary(),
        t.elapsed(),
        index.symbol_count(),
        index.module_count()
    );
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("lib.rpidx");
    let t = Instant::now();
    let size = IndexStore::save(&path, &index).unwrap();
    eprintln!("save: {:.1} MB in {:?}", size as f64 / 1e6, t.elapsed());
    // How well the body compresses (the decoder refuses more than MAX_RATIO).
    let bytes = std::fs::read(&path).unwrap();
    let meta_len = u32::from_le_bytes(bytes[16..20].try_into().unwrap()) as usize;
    let mut body = Vec::new();
    std::io::Read::read_to_end(
        &mut flate2::read::ZlibDecoder::new(&bytes[20 + meta_len..]),
        &mut body,
    )
    .unwrap();
    let ratio = body.len() as f64 / (bytes.len() - 20 - meta_len) as f64;
    eprintln!(
        "body: {:.1} MB, compressed {ratio:.1}x (limit {}x)",
        body.len() as f64 / 1e6,
        rustpy_index::format::MAX_RATIO
    );
    assert!(
        ratio * 2.0 < rustpy_index::format::MAX_RATIO as f64,
        "real indexes stay well below the limit"
    );
    let t = Instant::now();
    let loaded = IndexStore::load(&path).unwrap();
    eprintln!("load: {:?}", t.elapsed());
    assert!(loaded.same_content(&index));
    let t = Instant::now();
    let (same, again) = refresh(
        Some(&loaded),
        IndexMeta::default(),
        &roots,
        files,
        &[],
        &opts,
        &progress,
    )
    .unwrap();
    assert!(same.is_none());
    eprintln!(
        "refresh without changes: {} in {:?}",
        again.summary(),
        t.elapsed()
    );
    assert!(!again.changed());
    for q in [
        "JSONDecoder",
        "DataFrame",
        "get",
        "a",
        "ndarray",
        "Session.request",
    ] {
        let t = Instant::now();
        let hits = loaded.search(q, &SearchOptions::default(), &score);
        let el = t.elapsed();
        eprintln!(
            "search {q:?}: {} hits in {el:?}: {:?}",
            hits.len(),
            hits.iter()
                .take(4)
                .map(|h| format!("{}:{}.{} {}", h.module, h.container, h.name, h.detail))
                .collect::<Vec<_>>()
        );
    }
    // The scorer Search Everywhere uses.
    for q in [
        "a",
        "ge",
        "get",
        "gtattr",
        "JSONDecoder",
        "DataFrame",
        "Session.request",
        "np",
        "xyzzy",
    ] {
        let t = Instant::now();
        let hits = loaded.search(q, &SearchOptions::default(), &rustpy_search::fuzzy_score);
        eprintln!(
            "fuzzy search {q:?}: {} hits in {:?}: {:?}",
            hits.len(),
            t.elapsed(),
            hits.iter()
                .take(3)
                .map(|h| format!("{}:{}.{}", h.module, h.container, h.name))
                .collect::<Vec<_>>()
        );
    }
    // What second-press completion asks (prefix + completion-style matching).
    for q in ["s", "da", "sess", "json"] {
        let t = Instant::now();
        let c = loaded.import_candidates(
            q,
            &|n: &str| {
                let l = n.to_lowercase();
                l.contains(q)
            },
            200,
        );
        eprintln!(
            "completion candidates {q:?}: {} in {:?}",
            c.len(),
            t.elapsed()
        );
    }
    for q in ["Path", "DataFrame", "Session", "missing_name"] {
        let t = Instant::now();
        let c = loaded.exact_import_candidates(q, 8);
        eprintln!(
            "exact {q:?}: {:?} in {:?}",
            c.iter().map(|c| c.module.as_str()).collect::<Vec<_>>(),
            t.elapsed()
        );
    }
    for q in ["dataf", "ndarr", "sess", "a", "json"] {
        let t = Instant::now();
        let c = loaded.import_candidates(q, &|n: &str| n.to_lowercase().contains(q), 200);
        let el = t.elapsed();
        eprintln!(
            "import {q:?}: {} in {el:?}: {:?}",
            c.len(),
            c.iter()
                .take(6)
                .map(|c| format!("from {} import {} ({:?})", c.module, c.name, c.kind))
                .collect::<Vec<_>>()
        );
    }
}

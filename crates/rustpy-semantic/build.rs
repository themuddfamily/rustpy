//! Embeds the bundled typeshed stdlib stubs into the binary.

use std::fmt::Write;
use std::path::Path;

fn main() {
    let manifest = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let stdlib = Path::new(&manifest).join("typeshed").join("stdlib");
    println!("cargo:rerun-if-changed={}", stdlib.display());
    let mut files = Vec::new();
    collect(&stdlib, &stdlib, &mut files);
    files.sort();
    let mut out = String::new();
    writeln!(
        out,
        "/// `(relative path, contents)` of every bundled stub, sorted by path."
    )
    .unwrap();
    writeln!(out, "pub static TYPESHED_STDLIB: &[(&str, &str)] = &[").unwrap();
    for rel in &files {
        let abs = stdlib.join(rel);
        writeln!(
            out,
            "    ({:?}, include_str!({:?})),",
            rel.replace('\\', "/"),
            abs.display().to_string()
        )
        .unwrap();
    }
    writeln!(out, "];").unwrap();
    let third_party = Path::new(&manifest).join("typeshed").join("stubs");
    println!("cargo:rerun-if-changed={}", third_party.display());
    let mut third_files = Vec::new();
    collect(&third_party, &third_party, &mut third_files);
    third_files.sort();
    writeln!(out, "pub static TYPESHED_THIRD_PARTY: &[(&str, &str)] = &[").unwrap();
    for rel in &third_files {
        if !rel.ends_with(".pyi") {
            continue;
        }
        let normalized = rel.replace('\\', "/");
        let Some((_, module_path)) = normalized.split_once('/') else {
            continue;
        };
        writeln!(
            out,
            "    ({:?}, include_str!({:?})),",
            module_path,
            third_party.join(rel).display().to_string()
        )
        .unwrap();
    }
    writeln!(out, "];").unwrap();
    let dest = Path::new(&std::env::var("OUT_DIR").unwrap()).join("typeshed_data.rs");
    std::fs::write(dest, out).unwrap();
}

fn collect(root: &Path, dir: &Path, out: &mut Vec<String>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            if p.file_name().and_then(|n| n.to_str()) != Some("@tests") {
                collect(root, &p, out);
            }
        } else if p.extension().and_then(|e| e.to_str()) == Some("pyi")
            || p.file_name().and_then(|n| n.to_str()) == Some("VERSIONS")
        {
            out.push(p.strip_prefix(root).unwrap().to_string_lossy().into_owned());
        }
    }
}

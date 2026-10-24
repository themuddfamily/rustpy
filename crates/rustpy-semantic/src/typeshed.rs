//! Bundled typeshed stdlib stubs (roadmap §2.2 "Stubs").

use std::collections::HashMap;

use once_cell::sync::Lazy;

include!(concat!(env!("OUT_DIR"), "/typeshed_data.rs"));

static BY_PATH: Lazy<HashMap<&'static str, &'static str>> =
    Lazy::new(|| TYPESHED_STDLIB.iter().copied().collect());

static THIRD_PARTY_BY_PATH: Lazy<HashMap<&'static str, &'static str>> =
    Lazy::new(|| TYPESHED_THIRD_PARTY.iter().copied().collect());

/// Whether a bundled module belongs to the standard library catalogue.
pub fn is_stdlib(module: &str) -> bool {
    let rel = module.replace('.', "/");
    BY_PATH.contains_key(format!("{rel}.pyi").as_str())
        || BY_PATH.contains_key(format!("{rel}/__init__.pyi").as_str())
}

/// Python version as `(major, minor)`.
pub type PyVersion = (u32, u32);

/// Parsed `VERSIONS` file: module → (min version, max version).
static VERSIONS: Lazy<HashMap<&'static str, (PyVersion, Option<PyVersion>)>> = Lazy::new(|| {
    let mut map = HashMap::new();
    let Some(text) = BY_PATH.get("VERSIONS") else {
        return map;
    };
    for line in text.lines() {
        let line = line.split('#').next().unwrap_or("").trim();
        let Some((module, range)) = line.split_once(':') else {
            continue;
        };
        let range = range.trim();
        let (lo, hi) = range.split_once('-').unwrap_or((range, ""));
        let parse = |s: &str| -> Option<PyVersion> {
            let (a, b) = s.trim().split_once('.')?;
            Some((a.parse().ok()?, b.parse().ok()?))
        };
        if let Some(lo) = parse(lo) {
            map.insert(module, (lo, parse(hi)));
        }
    }
    map
});

/// Returns the stub source for a dotted module name, e.g. `os.path`.
pub fn stub_for_module(module: &str) -> Option<&'static str> {
    let rel = module.replace('.', "/");
    BY_PATH
        .get(format!("{rel}.pyi").as_str())
        .copied()
        .or_else(|| BY_PATH.get(format!("{rel}/__init__.pyi").as_str()).copied())
        .or_else(|| {
            THIRD_PARTY_BY_PATH
                .get(format!("{rel}.pyi").as_str())
                .copied()
        })
        .or_else(|| {
            THIRD_PARTY_BY_PATH
                .get(format!("{rel}/__init__.pyi").as_str())
                .copied()
        })
}

/// Whether the stub for `module` is a package (`__init__.pyi`).
pub fn is_package(module: &str) -> bool {
    let rel = module.replace('.', "/");
    BY_PATH.contains_key(format!("{rel}/__init__.pyi").as_str())
        || THIRD_PARTY_BY_PATH.contains_key(format!("{rel}/__init__.pyi").as_str())
}

/// Whether a stdlib module exists for the given Python version.
pub fn module_available(module: &str, version: PyVersion) -> bool {
    // VERSIONS lists top-level modules and some submodules; check the longest prefix.
    let mut name = module;
    loop {
        if let Some((lo, hi)) = VERSIONS.get(name) {
            return version >= *lo && hi.map(|h| version <= h).unwrap_or(true);
        }
        match name.rfind('.') {
            Some(i) => name = &name[..i],
            None => return stub_for_module(module).is_some(),
        }
    }
}

/// All top-level stdlib module names.
pub fn top_level_modules() -> impl Iterator<Item = &'static str> {
    VERSIONS.keys().copied().filter(|m| !m.contains('.')).chain(
        THIRD_PARTY_BY_PATH
            .keys()
            .filter_map(|path| path.split('/').next())
            .map(|name| name.trim_end_matches(".pyi")),
    )
}

/// All bundled stub module names (dotted), e.g. for completion.
pub fn all_modules() -> Vec<String> {
    let mut v: Vec<String> = TYPESHED_STDLIB
        .iter()
        .chain(TYPESHED_THIRD_PARTY.iter())
        .filter(|(p, _)| p.ends_with(".pyi"))
        .map(|(p, _)| {
            p.trim_end_matches(".pyi")
                .trim_end_matches("/__init__")
                .replace('/', ".")
        })
        .collect();
    v.sort();
    v.dedup();
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_stubs() {
        assert!(stub_for_module("builtins").unwrap().contains("class int"));
        assert!(stub_for_module("os.path").is_some());
        assert!(is_package("os"));
        assert!(!is_package("bisect"));
        assert!(stub_for_module("nonexistent_module_xyz").is_none());
        assert!(module_available("asyncio", (3, 12)));
        assert!(!module_available("distutils", (3, 12)));
        assert!(module_available("tomllib", (3, 11)));
        assert!(!module_available("tomllib", (3, 10)));
        assert!(top_level_modules().count() > 200);
    }
}

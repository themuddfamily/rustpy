//! Bounded SchemaStore cache. Rendering only requests work and clones snapshots;
//! HTTPS/proxy/certificate policy is the shared RustPy network authority.
use rustpy_core::TaskManager;
use std::{
    collections::HashMap,
    io::Read,
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, Instant},
};
#[derive(Default)]
struct State {
    entries: HashMap<String, (Instant, String)>,
    generation: u64,
    snapshot: Arc<Vec<(String, String)>>,
    references: HashMap<String, Vec<String>>,
}
static CACHE: OnceLock<Mutex<State>> = OnceLock::new();
static ACTIVE: Mutex<bool> = Mutex::new(false);
fn allowed(url: &str) -> bool {
    [
        "https://json.schemastore.org/",
        "https://www.schemastore.org/",
        "https://schemastore.org/",
    ]
    .iter()
    .any(|prefix| url.starts_with(*prefix))
        && !url.contains(['\r', '\n', '@'])
}
/// Snapshot identity changes only when cache contents change; clones never copy
/// schema megabytes into each render-frame analysis input.
pub fn snapshot() -> (u64, Arc<Vec<(String, String)>>) {
    let Some(cache) = CACHE.get() else {
        return (0, crate::data_schemas::builtin_values());
    };
    let Ok(cache) = cache.try_lock() else {
        return (0, crate::data_schemas::builtin_values());
    };
    (
        cache.generation,
        if cache.snapshot.is_empty() {
            crate::data_schemas::builtin_values()
        } else {
            cache.snapshot.clone()
        },
    )
}
pub fn request(url: &str, trusted: bool) {
    if !trusted || !allowed(url) || url.len() > 2048 {
        return;
    }
    let cache = CACHE.get_or_init(Default::default);
    let url = if let Ok(state) = cache.try_lock() {
        let mut queue = vec![url.to_string()];
        let mut seen = std::collections::HashSet::new();
        let mut missing = None;
        while let Some(candidate) = queue.pop() {
            if seen.len() >= 128 {
                break;
            }
            if !seen.insert(candidate.clone()) {
                continue;
            }
            if state
                .entries
                .get(&candidate)
                .is_none_or(|(at, _)| at.elapsed() >= Duration::from_secs(3600))
            {
                missing = Some(candidate);
                break;
            }
            if let Some(references) = state.references.get(&candidate) {
                queue.extend(references.iter().cloned());
            }
        }
        let Some(url) = missing else {
            return;
        };
        url
    } else {
        return;
    };
    let Ok(mut active) = ACTIVE.try_lock() else {
        return;
    };
    if *active {
        return;
    }
    *active = true;
    drop(active);
    TaskManager::global()
        .task("Loading JSON schema")
        .spawn(move |progress| {
            struct Release;
            impl Drop for Release {
                fn drop(&mut self) {
                    if let Ok(mut active) = ACTIVE.lock() {
                        *active = false;
                    }
                }
            }
            let _release = Release;
            let mut source = String::new();
            progress.check()?;
            let response = rustpy_net::agent_builder(&url)
                .timeout(Duration::from_secs(3))
                .redirects(0)
                .build()
                .get(&url)
                .call();
            if let Ok(response) = response {
                let _ = response
                    .into_reader()
                    .take(2 * 1024 * 1024 + 1)
                    .read_to_string(&mut source);
            }
            progress.check()?;
            if source.len() > 2 * 1024 * 1024
                || (serde_json::from_str::<serde_json::Value>(&source).is_err()
                    && !source.trim_start().starts_with('<'))
            {
                source.clear();
            }
            if let Ok(mut cache) = cache.lock() {
                if cache.entries.len() >= 12 {
                    cache.entries.clear();
                    cache.references.clear();
                }
                let references = references(&url, &source);
                cache.references.insert(url.clone(), references);
                cache.entries.insert(url, (Instant::now(), source));
                cache.generation = cache.generation.saturating_add(1);
                let mut values = cache
                    .entries
                    .iter()
                    .filter(|(_, (_, source))| !source.is_empty())
                    .map(|(url, (_, source))| (url.clone(), source.clone()))
                    .collect::<Vec<_>>();
                values.sort_by(|a, b| a.0.cmp(&b.0));
                values.extend(crate::data_schemas::builtin_values().iter().cloned());
                cache.snapshot = Arc::new(values);
            }
            Ok(())
        });
}
/// Explicit schema reference wins; standard filenames use their maintained
/// SchemaStore schemas without writing a $schema key into user data.
pub fn schema_url(source: &str, path: &std::path::Path) -> Option<String> {
    if source.len() > 2 * 1024 * 1024 {
        return None;
    }
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(source) {
        if let Some(url) = value.get("$schema").and_then(|v| v.as_str()) {
            return allowed(url).then(|| url.to_string());
        }
    }
    let name = path.file_name()?.to_str()?;
    let schema = match name {
        "package.json" => "package.json",
        "tsconfig.json" | "jsconfig.json" => "tsconfig.json",
        ".eslintrc.json" => "eslintrc.json",
        ".prettierrc.json" => "prettierrc.json",
        "composer.json" => "composer.json",
        "renovate.json" => "renovate.json",
        _ => return None,
    };
    Some(format!("https://json.schemastore.org/{schema}"))
}

fn references(base: &str, source: &str) -> Vec<String> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(source) else {
        return Vec::new();
    };
    let mut work = vec![&value];
    let mut out = std::collections::HashSet::new();
    let mut remaining = 4096;
    while let Some(value) = work.pop() {
        if remaining == 0 {
            break;
        }
        remaining -= 1;
        match value {
            serde_json::Value::Object(values) => {
                if let Some(reference) = values
                    .get("$ref")
                    .and_then(|v| v.as_str())
                    .filter(|v| !v.starts_with('#'))
                {
                    let reference = reference.split('#').next().unwrap_or("");
                    let url = if reference.starts_with("https://") {
                        reference.to_string()
                    } else {
                        format!(
                            "{}{reference}",
                            base.rsplit_once('/')
                                .map(|(prefix, _)| format!("{prefix}/"))
                                .unwrap_or_default()
                        )
                    };
                    if allowed(&url) {
                        out.insert(url);
                        if out.len() >= 128 {
                            break;
                        }
                    }
                }
                if work.len() + values.len() <= 4096 {
                    work.extend(values.values());
                }
            }
            serde_json::Value::Array(values) if work.len() + values.len() <= 4096 => {
                work.extend(values);
            }
            _ => {}
        }
    }
    out.into_iter().collect()
}
/// Explicit local schema loading shares the bounded catalogue and worker gate.
pub fn request_resource(resource: &str, trusted: bool) {
    if resource.starts_with("rustpy://") {
        return;
    }
    if resource.starts_with("https://") {
        request(resource, trusted);
        return;
    }
    if !trusted {
        return;
    }
    let path = if resource.starts_with("file://") {
        let Some(path) = url::Url::parse(resource)
            .ok()
            .and_then(|url| url.to_file_path().ok())
        else {
            return;
        };
        path
    } else if resource.contains("://") {
        return;
    } else {
        std::path::PathBuf::from(resource)
    };
    let key = resource.to_owned();
    let cache = CACHE.get_or_init(Default::default);
    if cache.try_lock().ok().is_some_and(|state| {
        state
            .entries
            .get(&key)
            .is_some_and(|(at, _)| at.elapsed() < Duration::from_secs(5))
    }) {
        return;
    }
    let Ok(mut active) = ACTIVE.try_lock() else {
        return;
    };
    if *active {
        return;
    }
    *active = true;
    drop(active);
    TaskManager::global()
        .task("Loading custom data schema")
        .spawn(move |progress| {
            struct Release;
            impl Drop for Release {
                fn drop(&mut self) {
                    if let Ok(mut active) = ACTIVE.lock() {
                        *active = false;
                    }
                }
            }
            let _release = Release;
            progress.check()?;
            let mut source = String::new();
            if let Ok(file) = std::fs::File::open(&path) {
                let _ = file.take(2 * 1024 * 1024 + 1).read_to_string(&mut source);
            }
            progress.check()?;
            if source.len() > 2 * 1024 * 1024
                || (serde_json::from_str::<serde_json::Value>(&source).is_err()
                    && !source.trim_start().starts_with('<'))
            {
                source.clear();
            }
            if let Ok(mut state) = cache.lock() {
                if state.entries.len() >= 32 {
                    state.entries.clear();
                }
                let references = local_references(&path, &source);
                state.entries.insert(key, (Instant::now(), source));
                for (key, source) in references {
                    state.entries.insert(key, (Instant::now(), source));
                }
                state.generation = state.generation.wrapping_add(1);
                let mut values = state
                    .entries
                    .iter()
                    .filter(|(_, (_, source))| !source.is_empty())
                    .map(|(key, (_, source))| (key.clone(), source.clone()))
                    .collect::<Vec<_>>();
                values.extend(crate::data_schemas::builtin_values().iter().cloned());
                values.sort_by(|a, b| a.0.cmp(&b.0));
                state.snapshot = Arc::new(values);
            }
            Ok(())
        });
}

fn local_references(path: &std::path::Path, source: &str) -> Vec<(String, String)> {
    let Some(directory) = path
        .parent()
        .and_then(|path| dunce::canonicalize(path).ok())
    else {
        return Vec::new();
    };
    let mut pending = vec![(path.to_path_buf(), source.to_owned())];
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    let mut bytes = 0usize;
    while let Some((base, source)) = pending.pop() {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&source) else {
            continue;
        };
        let mut nodes = vec![&value];
        let mut budget = 4096;
        while let Some(node) = nodes.pop() {
            if budget == 0 {
                break;
            }
            budget -= 1;
            if let Some(reference) = node
                .get("$ref")
                .and_then(|value| value.as_str())
                .filter(|reference| !reference.starts_with('#') && !reference.contains("://"))
            {
                let Some(path) = base
                    .parent()
                    .map(|parent| parent.join(reference.split('#').next().unwrap_or_default()))
                    .and_then(|path| dunce::canonicalize(&path).ok())
                    .filter(|path| path.starts_with(&directory))
                else {
                    continue;
                };
                if seen.insert(path.clone()) && seen.len() <= 32 {
                    let mut source = String::new();
                    if let Ok(file) = std::fs::File::open(&path) {
                        let _ = file.take(2 * 1024 * 1024 + 1).read_to_string(&mut source);
                    }
                    bytes += source.len();
                    if bytes > 8 * 1024 * 1024 {
                        return out;
                    }
                    if source.len() <= 2 * 1024 * 1024
                        && serde_json::from_str::<serde_json::Value>(&source).is_ok()
                    {
                        if let Ok(key) = url::Url::from_file_path(&path) {
                            out.push((key.to_string(), source.clone()));
                            pending.push((path, source));
                        }
                    }
                }
            }
            match node {
                serde_json::Value::Object(values) => nodes.extend(values.values()),
                serde_json::Value::Array(values) => nodes.extend(values),
                _ => {}
            }
            if nodes.len() > 4096 {
                break;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn schema_origins_are_explicit_and_no_arbitrary_uri_is_admitted() {
        assert_eq!(
            schema_url(
                r#"{"$schema":"https://json.schemastore.org/package.json"}"#,
                std::path::Path::new("p.json")
            )
            .as_deref(),
            Some("https://json.schemastore.org/package.json")
        );
        assert!(schema_url(
            r#"{"$schema":"file:///etc/passwd"}"#,
            std::path::Path::new("p.json")
        )
        .is_none());
        assert!(schema_url(
            r#"{"$schema":"https://json.schemastore.org.evil.test/x"}"#,
            std::path::Path::new("p.json")
        )
        .is_none());
        assert!(!allowed("https://json.schemastore.org/@evil.test"));
    }
}

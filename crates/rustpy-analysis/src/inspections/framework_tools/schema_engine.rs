//! JSON Schema draft semantics are delegated to the maintained validator;
//! retrieval is restricted to immutable captured resources (never network/IO).
use super::documents::push;
use super::{super::Problem, SchemaSnapshot};
use rustpy_core::{TextRange, TextSize};
use std::{
    collections::HashMap,
    sync::atomic::{AtomicUsize, Ordering},
};
struct Captured {
    values: HashMap<String, serde_json::Value>,
    retrievals: AtomicUsize,
}
impl jsonschema::Retrieve for Captured {
    fn retrieve(
        &self,
        uri: &jsonschema::Uri<String>,
    ) -> Result<serde_json::Value, Box<dyn std::error::Error + Send + Sync>> {
        if self.retrievals.fetch_add(1, Ordering::Relaxed) > 32 {
            return Err(std::io::Error::other("schema reference budget exceeded").into());
        }
        self.values
            .get(uri.as_ref().split('#').next().unwrap_or(""))
            .cloned()
            .ok_or_else(|| {
                std::io::Error::other("schema reference is not in the captured catalogue").into()
            })
    }
}
fn admit(value: &serde_json::Value) -> bool {
    let mut nodes = vec![(value, 0)];
    let mut count = 0;
    let mut bytes = 0usize;
    while let Some((value, depth)) = nodes.pop() {
        count += 1;
        if count > 4096 || depth > 32 {
            return false;
        }
        match value {
            serde_json::Value::Array(values) => {
                if nodes.len() + values.len() + count > 4096 {
                    return false;
                }
                nodes.extend(values.iter().map(|v| (v, depth + 1)));
            }
            serde_json::Value::Object(values) => {
                if nodes.len() + values.len() + count > 4096 {
                    return false;
                }
                for (key, value) in values {
                    bytes = bytes.saturating_add(key.len());
                    nodes.push((value, depth + 1));
                }
            }
            serde_json::Value::String(value) => bytes = bytes.saturating_add(value.len()),
            _ => {}
        }
        if bytes > 2 * 1024 * 1024 {
            return false;
        }
    }
    true
}
pub(super) fn inspect(
    source: &str,
    schema: &serde_json::Value,
    base: Option<&str>,
    snapshot: &SchemaSnapshot,
) -> Vec<Problem> {
    if source.len() > 2 * 1024 * 1024 || !admit(schema) {
        return Vec::new();
    }
    let Ok(value) = serde_json::from_str::<serde_json::Value>(source) else {
        return Vec::new();
    };
    if !admit(&value) {
        return Vec::new();
    }
    let resources = snapshot
        .values
        .iter()
        .take(44)
        .filter_map(|(url, source)| {
            if source.len() > 2 * 1024 * 1024 {
                return None;
            }
            let value = serde_json::from_str(source).ok()?;
            admit(&value).then(|| (url.clone(), value))
        })
        .collect();
    let options = jsonschema::options()
        .with_retriever(Captured {
            values: resources,
            retrievals: AtomicUsize::new(0),
        })
        .with_pattern_options(
            jsonschema::PatternOptions::fancy_regex()
                .backtrack_limit(10000)
                .size_limit(1024 * 1024)
                .dfa_size_limit(1024 * 1024),
        );
    let options = if let Some(base) = base {
        options.with_base_uri(base)
    } else {
        options
    };
    let Ok(validator) = options.build(schema) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for error in validator.iter_errors(&value).take(128) {
        let message = error.to_string();
        if message.to_lowercase().contains("backtrack") {
            continue;
        }
        push(
            &mut out,
            TextRange::empty(TextSize::new(0)),
            "JsonSchema",
            format!("{}: {message}", error.instance_path),
        );
    }
    out
}

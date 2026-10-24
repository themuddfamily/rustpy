//! Built-in configuration schema hints and explicit user mappings.
use std::{
    path::Path,
    sync::{Arc, OnceLock},
};
pub fn builtin_values() -> Arc<Vec<(String, String)>> {
    static VALUES: OnceLock<Arc<Vec<(String, String)>>> = OnceLock::new();
    VALUES.get_or_init(||{use serde_json::json;let definitions=[("github",json!({"type":"object","properties":{"name":{"type":"string"},"on":{},"permissions":{},"env":{"type":"object"},"concurrency":{},"defaults":{},"jobs":{"type":"object","additionalProperties":{"type":"object","properties":{"name":{"type":"string"},"runs-on":{},"needs":{},"if":{},"strategy":{},"uses":{"type":"string"},"steps":{"type":"array","items":{"type":"object","properties":{"name":{"type":"string"},"uses":{"type":"string"},"run":{"type":"string"},"with":{"type":"object"},"env":{"type":"object"},"working-directory":{"type":"string"},"shell":{"type":"string"}}}}}}}},"required":["jobs"]})),("compose",json!({"type":"object","properties":{"name":{"type":"string"},"version":{"type":"string"},"services":{"type":"object","additionalProperties":{"type":"object","properties":{"image":{"type":"string"},"build":{},"command":{},"ports":{"type":"array"},"environment":{},"volumes":{"type":"array"},"depends_on":{},"networks":{},"healthcheck":{"type":"object"},"restart":{"type":"string"}}}},"volumes":{"type":"object"},"networks":{"type":"object"},"configs":{"type":"object"},"secrets":{"type":"object"}},"required":["services"]})),("kubernetes",json!({"type":"object","properties":{"apiVersion":{"type":"string"},"kind":{"type":"string"},"metadata":{"type":"object","properties":{"name":{"type":"string"},"namespace":{"type":"string"},"labels":{"type":"object"},"annotations":{"type":"object"}}},"spec":{"type":"object"},"data":{"type":"object"},"stringData":{"type":"object"},"items":{"type":"array"},"type":{"type":"string"}},"required":["apiVersion","kind","metadata"]})),("precommit",json!({"type":"object","properties":{"repos":{"type":"array","items":{"type":"object","properties":{"repo":{"type":"string"},"rev":{"type":"string"},"hooks":{"type":"array","items":{"type":"object","properties":{"id":{"type":"string"},"name":{"type":"string"},"entry":{"type":"string"},"language":{"type":"string"},"args":{"type":"array"},"files":{"type":"string"},"exclude":{"type":"string"},"additional_dependencies":{"type":"array"}}}}},"required":["repo","hooks"]}},"default_language_version":{"type":"object"},"default_stages":{"type":"array"},"fail_fast":{"type":"boolean"}},"required":["repos"]})),("openapi",json!({"type":"object","properties":{"openapi":{"type":"string"},"swagger":{"type":"string"},"info":{"type":"object","properties":{"title":{"type":"string"},"version":{"type":"string"},"description":{"type":"string"}}},"paths":{"type":"object"},"components":{"type":"object"},"servers":{"type":"array"},"tags":{"type":"array"},"security":{"type":"array"},"externalDocs":{"type":"object"}},"required":["info","paths"]}))];Arc::new(definitions.into_iter().map(|(name,value)|(format!("rustpy://schemas/{name}"),value.to_string())).collect())}).clone()
}
pub fn resource(
    source: &str,
    path: &Path,
    mappings: &[(String, String)],
    root: Option<&Path>,
) -> Option<String> {
    if let Some((_, schema)) = mappings
        .iter()
        .find(|(pattern, _)| rustpy_settings::data_files::matches(pattern, path))
    {
        return Some(if schema.contains("://") {
            schema.clone()
        } else {
            let schema = Path::new(schema);
            let resolved = if schema.is_absolute() {
                schema.to_path_buf()
            } else {
                root.unwrap_or_else(|| path.parent().unwrap_or(Path::new(".")))
                    .join(schema)
            };
            url::Url::from_file_path(&resolved)
                .map(|url| url.to_string())
                .unwrap_or_else(|_| resolved.to_string_lossy().into_owned())
        });
    }
    let parsed_schema = crate::data_formats::value(source, path)
        .ok()
        .and_then(|parsed| {
            parsed
                .value
                .get("$schema")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        });
    // Keep mappings active while a JSON document is temporarily incomplete.
    // Only decode a bounded, literal top-level declaration; never evaluate input.
    let declared = parsed_schema.or_else(|| {
        source.lines().take(100).find_map(|line| {
            let line = line.trim().trim_start_matches('{').trim();
            let tail = line
                .strip_prefix("\"$schema\"")?
                .trim()
                .strip_prefix(':')?
                .trim();
            let mut decoder = serde_json::Deserializer::from_str(tail).into_iter::<String>();
            decoder.next()?.ok()
        })
    });
    if let Some(schema) = declared {
        if schema.contains("://") {
            return Some(schema);
        }
        let resolved = path.parent().unwrap_or(Path::new(".")).join(schema);
        return Some(
            url::Url::from_file_path(&resolved)
                .map(|url| url.to_string())
                .unwrap_or_else(|_| resolved.to_string_lossy().into_owned()),
        );
    }
    let name = path.file_name()?.to_str()?;
    let key = if path
        .components()
        .any(|part| part.as_os_str() == "workflows")
        && path.components().any(|part| part.as_os_str() == ".github")
    {
        Some("github")
    } else if matches!(
        name,
        "docker-compose.yml" | "docker-compose.yaml" | "compose.yml" | "compose.yaml"
    ) {
        Some("compose")
    } else if name == ".pre-commit-config.yaml" {
        Some("precommit")
    } else if name.starts_with("openapi") || name.starts_with("swagger") {
        Some("openapi")
    } else if source
        .lines()
        .take(100)
        .any(|line| line.starts_with("apiVersion:"))
        && source
            .lines()
            .take(100)
            .any(|line| line.starts_with("kind:"))
    {
        Some("kubernetes")
    } else {
        None
    };
    key.map(|key| format!("rustpy://schemas/{key}"))
        .or_else(|| crate::schema_store::schema_url(source, path))
}
pub fn schema(resource: &str, values: &[(String, String)]) -> Option<serde_json::Value> {
    values
        .iter()
        .find(|(key, _)| key == resource)
        .and_then(|(_, source)| serde_json::from_str(source).ok())
}
pub fn completion_schema<'a>(
    mut schema: &'a serde_json::Value,
    path: &str,
) -> &'a serde_json::Value {
    for part in path.split('/').skip(1) {
        let part = part.replace("~1", "/").replace("~0", "~");
        if part.parse::<usize>().is_ok() {
            if let Some(items) = schema.get("items") {
                schema = items;
            }
        } else if let Some(property) = schema
            .get("properties")
            .and_then(|properties| properties.get(&part))
            .or_else(|| {
                schema
                    .get("additionalProperties")
                    .filter(|value| value.is_object())
            })
        {
            schema = property;
        }
    }
    schema
}

#[cfg(test)]
mod fixtures {
    use super::*;
    #[test]
    fn local_mappings_override_document_schema_and_keep_uri_identity() {
        let mappings = vec![("*.json".into(), "schemas/local.json".into())];
        assert_eq!(
            resource(
                r#"{"$schema":"https://json.schemastore.org/package.json"}"#,
                Path::new("/work/package.json"),
                &mappings,
                Some(Path::new("/work"))
            )
            .as_deref(),
            Some("file:///work/schemas/local.json")
        );
        assert_eq!(
            resource(
                "{\n\"$schema\":\"schema.json\",\n incomplete",
                Path::new("/work/data.json"),
                &[],
                None
            )
            .as_deref(),
            Some("file:///work/schema.json")
        );
    }
    #[test]
    fn yaml_configuration_schema_keeps_on_key_and_nested_steps() {
        let resource = resource(
            "on: push\njobs: {}",
            Path::new("/work/.github/workflows/build.yml"),
            &[],
            None,
        )
        .unwrap();
        let values = builtin_values();
        let schema = schema(&resource, &values).unwrap();
        assert!(schema["properties"].get("on").is_some());
        assert!(
            completion_schema(&schema, "/jobs/build/steps/0")["properties"]
                .get("run")
                .is_some()
        );
    }
}

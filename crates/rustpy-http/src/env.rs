//! Environments (`http-client.env.json`, `http-client.private.env.json`) and
//! `{{variable}}` substitution, including the `{{$name}}` dynamic built-ins.

use std::collections::{BTreeSet, HashMap};
use std::hash::{BuildHasher, Hasher};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

pub const PUBLIC_ENV_FILE: &str = "http-client.env.json";
pub const PRIVATE_ENV_FILE: &str = "http-client.private.env.json";

fn random_u64() -> u64 {
    std::collections::hash_map::RandomState::new()
        .build_hasher()
        .finish()
}

fn uuid_v4() -> String {
    let a = random_u64();
    let b = random_u64();
    let mut bytes = [0u8; 16];
    bytes[..8].copy_from_slice(&a.to_le_bytes());
    bytes[8..].copy_from_slice(&b.to_le_bytes());
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..]
    )
}

/// Value of a built-in dynamic variable such as `$uuid`, `$timestamp`, `$randomInt`.
pub fn builtin_variable(name: &str) -> Option<String> {
    let name = name.trim();
    match name {
        "$uuid" | "$random.uuid" => Some(uuid_v4()),
        "$timestamp" => Some(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0)
                .to_string(),
        ),
        "$isoTimestamp" => {
            let secs = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            Some(iso_timestamp(secs))
        }
        "$randomInt" => Some((random_u64() % 1000).to_string()),
        _ => {
            if let Some(rest) = name.strip_prefix("$random.integer(") {
                let args = rest.trim_end_matches(')');
                let mut parts = args
                    .split(',')
                    .map(|p| p.trim().parse::<u64>().unwrap_or(0));
                let lo = parts.next().unwrap_or(0);
                let hi = parts.next().unwrap_or(lo + 1000).max(lo + 1);
                return Some((lo + random_u64() % (hi - lo)).to_string());
            }
            None
        }
    }
}

/// `YYYY-MM-DDTHH:MM:SSZ` for a unix timestamp (no chrono dependency).
fn iso_timestamp(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (h, m, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    // Civil-from-days (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mo = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if mo <= 2 { y + 1 } else { y };
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{m:02}:{s:02}Z")
}

/// Replaces `{{name}}` occurrences with environment values or built-ins.
/// Unknown variables are left untouched.
pub fn substitute(text: &str, vars: &HashMap<String, String>) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        match after.find("}}") {
            Some(end) => {
                let name = after[..end].trim();
                let value = if name.starts_with('$') {
                    builtin_variable(name)
                } else {
                    vars.get(name).cloned()
                };
                match value {
                    Some(v) => out.push_str(&v),
                    None => out.push_str(&rest[start..start + 2 + end + 2]),
                }
                rest = &after[end + 2..];
            }
            None => {
                out.push_str(&rest[start..]);
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out
}

fn read_env_file(path: &Path) -> Option<serde_json::Map<String, serde_json::Value>> {
    let snapshot = rustpy_core::vfs::Vfs::new()
        .read(
            path,
            rustpy_core::vfs::ReadLimits {
                max_file_bytes: 2 * 1024 * 1024,
                max_cached_bytes: 0,
                max_cached_files: 0,
            },
        )
        .ok()?;
    let text = std::str::from_utf8(&snapshot.bytes).ok()?;
    match serde_json::from_str::<serde_json::Value>(text) {
        Ok(serde_json::Value::Object(m)) => Some(m),
        _ => None,
    }
}

fn value_to_string(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// Variables of `env_name` from the public and private environment files in
/// `dir` (private values override public ones). Missing files yield an empty map.
pub fn load_env(dir: &Path, env_name: &str) -> HashMap<String, String> {
    let mut vars = HashMap::new();
    for file in [PUBLIC_ENV_FILE, PRIVATE_ENV_FILE] {
        let Some(map) = read_env_file(&dir.join(file)) else {
            continue;
        };
        if let Some(serde_json::Value::Object(env)) = map.get(env_name) {
            for (k, v) in env {
                vars.insert(k.clone(), value_to_string(v));
            }
        }
    }
    vars
}

/// Merge ancestor environments inside the captured project, nearest directory wins.
pub fn load_project_env(root: &Path, dir: &Path, name: &str) -> HashMap<String, String> {
    let mut dirs = vec![dir];
    if dir.starts_with(root) {
        dirs = dir
            .ancestors()
            .take_while(|directory| directory.starts_with(root))
            .take(64)
            .collect();
        dirs.reverse();
    }
    let mut variables = HashMap::new();
    for directory in dirs {
        variables.extend(load_env(directory, name));
    }
    variables
}
pub fn project_env_names(root: &Path, dir: &Path) -> Vec<String> {
    let mut names = BTreeSet::new();
    for directory in dir.ancestors().take(64) {
        names.extend(list_env_names(directory));
        if directory == root || !directory.starts_with(root) {
            break;
        }
    }
    names.into_iter().collect()
}

/// Environment names defined in `dir` (sorted, deduplicated).
pub fn list_env_names(dir: &Path) -> Vec<String> {
    let mut names = BTreeSet::new();
    for file in [PUBLIC_ENV_FILE, PRIVATE_ENV_FILE] {
        if let Some(map) = read_env_file(&dir.join(file)) {
            for (k, v) in &map {
                if v.is_object() {
                    names.insert(k.clone());
                }
            }
        }
    }
    names.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn substitutes_variables_and_builtins() {
        let mut vars = HashMap::new();
        vars.insert("host".to_string(), "example.com".to_string());
        assert_eq!(
            substitute("https://{{host}}/x/{{ host }}", &vars),
            "https://example.com/x/example.com"
        );
        assert_eq!(substitute("{{missing}}", &vars), "{{missing}}");
        assert_eq!(substitute("{{unterminated", &vars), "{{unterminated");
        let uuid = substitute("{{$uuid}}", &vars);
        assert_eq!(uuid.len(), 36);
        assert_eq!(uuid.as_bytes()[14], b'4');
        let ts: u64 = substitute("{{$timestamp}}", &vars).parse().unwrap();
        assert!(ts > 1_600_000_000);
        let n: u64 = substitute("{{$randomInt}}", &vars).parse().unwrap();
        assert!(n < 1000);
        let n: u64 = substitute("{{$random.integer(5, 7)}}", &vars)
            .parse()
            .unwrap();
        assert!((5..7).contains(&n));
        assert!(substitute("{{$isoTimestamp}}", &vars).ends_with('Z'));
    }

    #[test]
    fn iso_timestamp_epoch() {
        assert_eq!(iso_timestamp(0), "1970-01-01T00:00:00Z");
        assert_eq!(iso_timestamp(1_700_000_000), "2023-11-14T22:13:20Z");
    }

    #[test]
    fn loads_environments() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(PUBLIC_ENV_FILE), r#"{"dev": {"host": "localhost:8000", "port": 8000}, "prod": {"host": "api.example.com"}}"#).unwrap();
        std::fs::write(
            dir.path().join(PRIVATE_ENV_FILE),
            r#"{"dev": {"token": "secret", "host": "127.0.0.1"}}"#,
        )
        .unwrap();
        assert_eq!(
            list_env_names(dir.path()),
            vec!["dev".to_string(), "prod".to_string()]
        );
        let dev = load_env(dir.path(), "dev");
        assert_eq!(dev.get("host").map(String::as_str), Some("127.0.0.1"));
        assert_eq!(dev.get("token").map(String::as_str), Some("secret"));
        assert_eq!(dev.get("port").map(String::as_str), Some("8000"));
        assert!(load_env(dir.path(), "staging").is_empty());
        assert!(load_env(&dir.path().join("nope"), "dev").is_empty());
    }
}

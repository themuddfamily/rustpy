//! PyPI JSON API lookups (`https://pypi.org/pypi/<name>/json`).

use std::io::Read;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::requirements::normalize_name;
use crate::version::parse_version;

const PYPI_URL: &str = "https://pypi.org/pypi";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PypiSummary {
    pub name: String,
    pub version: String,
    pub summary: String,
}

/// Project metadata from PyPI.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PypiProject {
    pub name: String,
    /// Latest release.
    pub version: String,
    pub summary: String,
    pub home_page: String,
    pub author: String,
    pub license: String,
    pub requires_python: String,
    pub requires_dist: Vec<String>,
    /// All releases, newest first.
    pub versions: Vec<String>,
}

/// Agent for PyPI with the IDE's proxy and certificate settings (`rustpy_net`).
fn agent() -> ureq::Agent {
    rustpy_net::agent_builder(PYPI_URL)
        .timeout_connect(Duration::from_secs(5))
        .timeout(Duration::from_secs(15))
        .user_agent("rustpy-ide/0.1 (+https://github.com/rustpy/rustpy)")
        .build()
}

/// Fetches the project JSON; `Ok(None)` when the project does not exist.
fn fetch_project_json_network(name: &str) -> Result<Option<serde_json::Value>> {
    let name = normalize_name(name);
    if name.is_empty()
        || !name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-')
    {
        bail!("invalid package name");
    }
    let url = format!("{PYPI_URL}/{name}/json");
    match agent().get(&url).set("Accept", "application/json").call() {
        Ok(resp) => {
            let mut text = String::new();
            resp.into_reader()
                .take(64 * 1024 * 1024)
                .read_to_string(&mut text)
                .context("cannot read PyPI response")?;
            let v: serde_json::Value =
                serde_json::from_str(&text).context("cannot parse PyPI JSON")?;
            Ok(Some(v))
        }
        Err(ureq::Error::Status(404, _)) => Ok(None),
        Err(ureq::Error::Status(code, resp)) => bail!(
            "PyPI returned HTTP {code} for {name}: {}",
            resp.status_text()
        ),
        Err(ureq::Error::Transport(t)) => bail!(
            "cannot reach PyPI: {}",
            rustpy_net::describe_transport_error(&t)
        ),
    }
}

fn cache_path(name: &str) -> Option<std::path::PathBuf> {
    let name = normalize_name(name);
    if name.is_empty()
        || !name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-')
    {
        return None;
    }
    Some(
        dirs::cache_dir()?
            .join("rustpy/pypi")
            .join(format!("{name}.json")),
    )
}

fn fetch_project_json(name: &str) -> Result<Option<serde_json::Value>> {
    let cached = cache_path(name);
    let read = || {
        cached
            .as_ref()
            .and_then(|path| std::fs::read(path).ok())
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
    };
    if cached
        .as_ref()
        .and_then(|path| path.metadata().ok())
        .and_then(|meta| meta.modified().ok())
        .and_then(|time| time.elapsed().ok())
        .is_some_and(|age| age < Duration::from_secs(3600))
    {
        if let Some(value) = read() {
            return Ok(Some(value));
        }
    }
    match fetch_project_json_network(name) {
        Ok(Some(value)) => {
            if let Some(path) = cached.as_ref() {
                if let Some(parent) = path.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                if let Ok(bytes) = serde_json::to_vec(&value) {
                    let _ = std::fs::write(path, bytes);
                }
            }
            Ok(Some(value))
        }
        Err(error) => read().map(Some).ok_or(error),
        result => result,
    }
}

/// Deterministic relevance ranking shared by live and offline catalog search.
pub fn rank_search_names(query: &str, names: &[String], limit: usize) -> Vec<String> {
    let query = normalize_name(query);
    if query.is_empty() {
        return Vec::new();
    }
    let mut matches: Vec<_> = names
        .iter()
        .filter_map(|name| {
            let normalized = normalize_name(name);
            let score = if normalized == query {
                0
            } else if normalized.starts_with(&query) {
                1
            } else if normalized.contains(&query) {
                2
            } else {
                return None;
            };
            Some((score, normalized.len(), name))
        })
        .collect();
    matches.sort();
    matches
        .into_iter()
        .take(limit.min(50))
        .map(|(_, _, name)| name.clone())
        .collect()
}

/// Search the public simple-index catalog, with persistent offline metadata.
pub fn search_catalog(query: &str) -> Result<Vec<PypiSummary>> {
    let path = cache_path("simple-index");
    let mut names: Vec<String> = path
        .as_ref()
        .and_then(|p| std::fs::read(p).ok())
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default();
    let stale = path
        .as_ref()
        .and_then(|p| p.metadata().ok())
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.elapsed().ok())
        .is_none_or(|age| age > Duration::from_secs(86400));
    if stale {
        if let Ok(response) = agent()
            .get("https://pypi.org/simple/")
            .set("Accept", "application/vnd.pypi.simple.v1+json")
            .call()
        {
            let mut text = String::new();
            if response
                .into_reader()
                .take(96 * 1024 * 1024)
                .read_to_string(&mut text)
                .is_ok()
            {
                if let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) {
                    if let Some(projects) = value["projects"].as_array() {
                        names = projects
                            .iter()
                            .filter_map(|p| p["name"].as_str().map(str::to_owned))
                            .collect();
                        if let Some(path) = path.as_ref() {
                            if let Some(parent) = path.parent() {
                                let _ = std::fs::create_dir_all(parent);
                            }
                            if let Ok(bytes) = serde_json::to_vec(&names) {
                                let _ = std::fs::write(path, bytes);
                            }
                        }
                    }
                }
            }
        }
    }
    let mut ranked = rank_search_names(query, &names, 8);
    if ranked.is_empty() {
        ranked.push(query.trim().to_string());
    }
    let mut results = Vec::new();
    for name in ranked {
        if let Ok(project) = pypi_project(&name) {
            results.push(PypiSummary {
                name: project.name,
                version: project.version,
                summary: project.summary,
            });
        }
    }
    if results.is_empty() {
        bail!("No matching packages available online or in the offline cache")
    }
    Ok(results)
}

fn str_field(info: &serde_json::Value, key: &str) -> String {
    info.get(key)
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}

/// Sorts release strings newest first (PEP 440 order).
fn sorted_versions(releases: &serde_json::Value) -> Vec<String> {
    let mut versions: Vec<String> = releases
        .as_object()
        .map(|m| {
            m.iter()
                .filter(|(_, files)| {
                    // Skip releases whose files were all yanked (or that have no files).
                    files.as_array().is_some_and(|fs| {
                        fs.iter()
                            .any(|f| !f.get("yanked").and_then(|y| y.as_bool()).unwrap_or(false))
                    })
                })
                .map(|(v, _)| v.clone())
                .collect()
        })
        .unwrap_or_default();
    versions.sort_by_key(|v| std::cmp::Reverse(parse_version(v)));
    versions
}

/// Full project information for an exact (normalized) name.
pub fn pypi_project(name: &str) -> Result<PypiProject> {
    let json = fetch_project_json(name)?
        .ok_or_else(|| anyhow!("package `{}` not found on PyPI", name.trim()))?;
    let info = json.get("info").cloned().unwrap_or_default();
    let versions = sorted_versions(json.get("releases").unwrap_or(&serde_json::Value::Null));
    let home_page = {
        let hp = str_field(&info, "home_page");
        if hp.is_empty() {
            info.get("project_urls")
                .and_then(|u| u.as_object())
                .and_then(|u| {
                    u.get("Homepage")
                        .or_else(|| u.get("Home").or_else(|| u.values().next()))
                })
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string()
        } else {
            hp
        }
    };
    let license = {
        let l = str_field(&info, "license_expression");
        if l.is_empty() {
            str_field(&info, "license")
        } else {
            l
        }
    };
    Ok(PypiProject {
        name: str_field(&info, "name"),
        version: str_field(&info, "version"),
        summary: str_field(&info, "summary"),
        home_page,
        author: str_field(&info, "author"),
        license,
        requires_python: str_field(&info, "requires_python"),
        requires_dist: info
            .get("requires_dist")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default(),
        versions,
    })
}

/// Looks a package up by exact name (PyPI has no search API any more): zero or
/// one result per name. Whitespace/comma separated queries look every name up.
/// `Err` only for network/transport problems.
pub fn search_pypi(query: &str) -> Result<Vec<PypiSummary>> {
    let mut out = Vec::new();
    for name in query
        .split(|c: char| c.is_whitespace() || c == ',')
        .filter(|s| !s.is_empty())
        .take(5)
    {
        if let Some(json) = fetch_project_json(name)? {
            let info = json.get("info").cloned().unwrap_or_default();
            let name = str_field(&info, "name");
            if !name.is_empty() && !out.iter().any(|s: &PypiSummary| s.name == name) {
                out.push(PypiSummary {
                    name,
                    version: str_field(&info, "version"),
                    summary: str_field(&info, "summary"),
                });
            }
        }
    }
    Ok(out)
}

/// Available (non-yanked) versions of a project, newest first.
pub fn pypi_versions(name: &str) -> Result<Vec<String>> {
    let json = fetch_project_json(name)?
        .ok_or_else(|| anyhow!("package `{}` not found on PyPI", name.trim()))?;
    Ok(sorted_versions(
        json.get("releases").unwrap_or(&serde_json::Value::Null),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sorts_release_keys_and_skips_yanked() {
        let releases = serde_json::json!({
            "1.0": [{"yanked": false}],
            "1.10": [{"yanked": false}],
            "1.2": [{"yanked": false}],
            "2.0rc1": [{"yanked": false}],
            "1.5": [{"yanked": true}],
            "0.1": []
        });
        assert_eq!(
            sorted_versions(&releases),
            vec!["2.0rc1", "1.10", "1.2", "1.0"]
        );
    }

    /// Needs network access: `cargo test -p rustpy-packages -- --ignored`.
    #[test]
    #[ignore]
    fn fetches_requests_from_pypi() {
        let p = pypi_project("Requests").unwrap();
        assert_eq!(p.name, "requests");
        assert!(!p.versions.is_empty());
        assert!(p.versions.iter().any(|v| v == "2.31.0"));
        assert_eq!(pypi_versions("requests").unwrap()[..3].len(), 3);
        assert!(search_pypi("definitely-not-a-package-name-xyz-123456")
            .unwrap()
            .is_empty());
        assert_eq!(search_pypi("requests").unwrap().len(), 1);
    }
}

//! Known-vulnerability checks against the OSV database (<https://osv.dev>)
//! for the "Vulnerable Dependencies" / Package Checker feature: installed
//! packages or `requirements.txt` entries are matched against the PyPI
//! advisories OSV aggregates (GitHub `GHSA-*`, PyPA `PYSEC-*`, OpenSSF
//! malicious-package `MAL-*` entries, …).
//!
//! A check sends one `querybatch` POST per 1000 packages (advisory ids per
//! package; `next_page_token`s are followed) and then one
//! `GET /v1/vulns/{id}` per advisory for the details (summary, severity,
//! affected ranges). Detail lookups are capped at [`MAX_DETAIL_LOOKUPS`] per
//! check, handed out to the vulnerable packages in turns and run on several
//! threads; advisories beyond the cap are still reported, with the id only.
//! Entries that describe the same problem (a `GHSA-*` advisory and its
//! `PYSEC-*` twin, or two entries for one CVE) are reported once. Every
//! request has a 10 second timeout and everything blocks, so call the checks
//! from a worker thread in the UI.

use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::requirements::{normalize_name, Requirement};
use crate::version::{parse_version, specifier_matches, Version};
use crate::InstalledPackage;

const OSV_QUERYBATCH_URL: &str = "https://api.osv.dev/v1/querybatch";
const OSV_VULN_URL: &str = "https://api.osv.dev/v1/vulns";
const USER_AGENT: &str = "rustpy-ide/0.1 (+https://github.com/rustpy/rustpy)";
/// Connect and overall timeout of every OSV request.
const TIMEOUT: Duration = Duration::from_secs(10);
/// OSV accepts at most 1000 queries per batch request.
const BATCH_SIZE: usize = 1000;
/// Batch pages requested per chunk before remaining `next_page_token`s are ignored.
const MAX_BATCH_PAGES: usize = 10;
/// Threads fetching advisory details concurrently.
const DETAIL_THREADS: usize = 8;
/// Upper bound on advisory detail lookups (`GET /v1/vulns/{id}`) per check;
/// advisories beyond it are reported with their id only.
pub const MAX_DETAIL_LOOKUPS: usize = 20;

/// One known vulnerability (OSV advisory) of a package.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Vulnerability {
    /// OSV id, e.g. `GHSA-j8r2-6x86-q33q` or `PYSEC-2023-74`.
    pub id: String,
    /// One line: the advisory's summary, else the start of its details.
    /// Empty when the details were not fetched (lookup cap reached or the
    /// lookup failed).
    pub summary: String,
    /// `CRITICAL`, `HIGH`, `MODERATE` or `LOW` (GitHub's vocabulary): the
    /// level the advisory states (GitHub's review), else the rating of its
    /// CVSS 3.x vector (scored per the specification). Failing those, a plain
    /// level among its scores, upper-cased (e.g. `MEDIUM`), or else its first
    /// score as given (e.g. a CVSS 4.0 vector, which is not scored here).
    /// `None` when unknown. Order with [`severity_rank`].
    pub severity: Option<String>,
    /// Affected ranges as PEP 440 specifier sets, e.g. `>=2.3.0,<2.31.0`,
    /// `<2.20.0` or `==1.0` (enumerated versions); a version is affected when
    /// it matches any of them ([`Vulnerability::affects`]). Empty when unknown.
    pub affected_versions: Vec<String>,
    /// Versions that fix the problem, oldest first.
    pub fixed_versions: Vec<String>,
    /// The most useful advisory link (GitHub advisory page, NVD entry, …);
    /// [`Vulnerability::osv_url`] always has a page.
    pub reference: Option<String>,
}

impl Vulnerability {
    /// The advisory's page on osv.dev.
    pub fn osv_url(&self) -> String {
        format!("https://osv.dev/vulnerability/{}", self.id)
    }

    /// True when `version` falls in an affected range. Without range
    /// information (details not fetched) every version counts as affected.
    pub fn affects(&self, version: &str) -> bool {
        if self.affected_versions.is_empty() {
            return true;
        }
        let v = parse_version(version);
        self.affected_versions
            .iter()
            .any(|spec| specifier_matches(&v, spec))
    }
}

/// Orders severity levels for sorting and filtering: `CRITICAL` 4, `HIGH` 3,
/// `MODERATE`/`MEDIUM` 2, `LOW` 1, anything else (unknown, raw vectors) 0.
/// Case-insensitive.
pub fn severity_rank(severity: &str) -> u8 {
    match severity.trim().to_ascii_uppercase().as_str() {
        "CRITICAL" => 4,
        "HIGH" => 3,
        "MODERATE" | "MEDIUM" => 2,
        "LOW" => 1,
        _ => 0,
    }
}

/// The target of an "Upgrade to …" quick fix: the oldest version newer than
/// `current` that one of `vulns` lists as fixed and that none of them
/// affects; a final release unless `current` is a pre-release or only
/// pre-releases qualify. Advisories without range information are ignored.
/// `None` when no listed fix clears them all.
pub fn suggested_upgrade(current: &str, vulns: &[Vulnerability]) -> Option<String> {
    let current = parse_version(current);
    let known: Vec<&Vulnerability> = vulns
        .iter()
        .filter(|v| !v.affected_versions.is_empty())
        .collect();
    let mut candidates: Vec<(Version, &str)> = known
        .iter()
        .flat_map(|v| &v.fixed_versions)
        .map(|f| (parse_version(f), f.as_str()))
        .filter(|(v, _)| v.valid && *v > current)
        .collect();
    candidates.sort_by(|a, b| a.0.cmp(&b.0));
    let clears = |fix: &str| known.iter().all(|v| !v.affects(fix));
    let pre_ok = current.is_prerelease();
    candidates
        .iter()
        .find(|(v, f)| (pre_ok || !v.is_prerelease()) && clears(f))
        .or_else(|| candidates.iter().find(|(_, f)| clears(f)))
        .map(|(_, f)| f.to_string())
}

// ---------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------

fn str_at<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or("")
}

fn array_at<'a>(value: &'a Value, key: &str) -> &'a [Value] {
    value
        .get(key)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
}

fn dedup_keep_order(items: &mut Vec<String>) {
    let mut seen = HashSet::new();
    items.retain(|x| seen.insert(x.clone()));
}

/// One `querybatch` result: advisory ids (deduplicated, in order) and the
/// token for the next page when OSV has more.
type BatchResult = (Vec<String>, Option<String>);

fn parse_batch_results(json: &str) -> Result<Vec<BatchResult>> {
    let doc: Value = serde_json::from_str(json).context("cannot parse OSV querybatch response")?;
    let results = doc
        .get("results")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("OSV querybatch response has no results"))?;
    Ok(results
        .iter()
        .map(|r| {
            let mut ids: Vec<String> = array_at(r, "vulns")
                .iter()
                .map(|v| str_at(v, "id").trim())
                .filter(|id| !id.is_empty())
                .map(str::to_string)
                .collect();
            dedup_keep_order(&mut ids);
            let token = Some(str_at(r, "next_page_token").trim())
                .filter(|t| !t.is_empty())
                .map(str::to_string);
            (ids, token)
        })
        .collect())
}

/// Parses a `querybatch` response into the vulnerability ids per query, in
/// query order (empty for queries without known vulnerabilities); page
/// tokens are dropped. Malformed input yields no results.
pub fn parse_osv_batch(json: &str) -> Vec<Vec<String>> {
    parse_batch_results(json)
        .map(|results| results.into_iter().map(|(ids, _)| ids).collect())
        .unwrap_or_default()
}

/// Affected ranges of one PyPI package named by an advisory.
#[derive(Clone, Debug, Default, PartialEq)]
struct AffectedPackage {
    /// PEP 503-normalized name (empty when the entry names none).
    name: String,
    /// Specifier sets as in [`Vulnerability::affected_versions`].
    ranges: Vec<String>,
    fixed: Vec<String>,
}

/// A parsed advisory: what [`Vulnerability`] shows, plus the aliases used to
/// collapse duplicates and the ranges per affected package.
#[derive(Clone, Debug, Default)]
struct Advisory {
    id: String,
    summary: String,
    severity: Option<String>,
    reference: Option<String>,
    /// Other ids of the same problem (CVE, twin `GHSA-*`/`PYSEC-*` entries).
    aliases: Vec<String>,
    withdrawn: bool,
    packages: Vec<AffectedPackage>,
}

impl Advisory {
    /// Placeholder for an advisory whose details were not fetched.
    fn stub(id: &str) -> Advisory {
        Advisory {
            id: id.to_string(),
            ..Default::default()
        }
    }

    fn has_details(&self) -> bool {
        !self.summary.is_empty() || self.severity.is_some() || !self.packages.is_empty()
    }

    /// The advisory as it applies to `package` (normalized name): that
    /// package's ranges, or those of every PyPI package it lists when
    /// `package` is `None` or not among them.
    fn to_vulnerability(&self, package: Option<&str>) -> Vulnerability {
        let named = |p: &&AffectedPackage| package.is_some_and(|name| p.name == name);
        let selected: Vec<&AffectedPackage> = if self.packages.iter().any(|p| named(&p)) {
            self.packages.iter().filter(named).collect()
        } else {
            self.packages.iter().collect()
        };
        let mut affected_versions: Vec<String> = selected
            .iter()
            .flat_map(|p| p.ranges.iter().cloned())
            .collect();
        let mut fixed_versions: Vec<String> = selected
            .iter()
            .flat_map(|p| p.fixed.iter().cloned())
            .collect();
        dedup_keep_order(&mut affected_versions);
        dedup_keep_order(&mut fixed_versions);
        fixed_versions.sort_by_cached_key(|v| parse_version(v));
        Vulnerability {
            id: self.id.clone(),
            summary: self.summary.clone(),
            severity: self.severity.clone(),
            affected_versions,
            fixed_versions,
            reference: self.reference.clone(),
        }
    }
}

/// Cuts `text` to at most `max_chars` characters at a word boundary, marking
/// the cut with an ellipsis.
fn shorten(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let head: String = text.chars().take(max_chars).collect();
    let cut = if text.chars().nth(max_chars).is_some_and(char::is_whitespace) {
        head.len()
    } else {
        head.rfind(char::is_whitespace)
            .filter(|&i| i > 0)
            .unwrap_or(head.len())
    };
    format!("{}…", head[..cut].trim_end())
}

/// The first paragraph of the advisory text (markdown headings skipped),
/// joined into one line and shortened, for advisories without a `summary`
/// (most `PYSEC-*` entries).
fn summary_from_details(details: &str) -> String {
    let paragraph: Vec<&str> = details
        .lines()
        .map(str::trim)
        .skip_while(|l| l.is_empty() || l.starts_with('#'))
        .take_while(|l| !l.is_empty() && !l.starts_with('#'))
        .collect();
    shorten(&paragraph.join(" "), 200)
}

/// Base score of a CVSS 3.0/3.1 vector string (`CVSS:3.1/AV:N/AC:L/…`), per
/// the specification's formulas; `None` for anything else.
fn cvss3_base_score(vector: &str) -> Option<f64> {
    let mut parts = vector.trim().split('/').filter(|p| !p.is_empty());
    let version = parts.next()?;
    if !(version.eq_ignore_ascii_case("CVSS:3.0") || version.eq_ignore_ascii_case("CVSS:3.1")) {
        return None;
    }
    let mut metrics: HashMap<&str, &str> = HashMap::new();
    for part in parts {
        let (k, v) = part.split_once(':')?;
        metrics.insert(k, v);
    }
    let get = |k: &str| metrics.get(k).copied();
    let changed = match get("S")? {
        "U" => false,
        "C" => true,
        _ => return None,
    };
    let av = match get("AV")? {
        "N" => 0.85,
        "A" => 0.62,
        "L" => 0.55,
        "P" => 0.2,
        _ => return None,
    };
    let ac = match get("AC")? {
        "L" => 0.77,
        "H" => 0.44,
        _ => return None,
    };
    let pr = match (get("PR")?, changed) {
        ("N", _) => 0.85,
        ("L", false) => 0.62,
        ("L", true) => 0.68,
        ("H", false) => 0.27,
        ("H", true) => 0.5,
        _ => return None,
    };
    let ui = match get("UI")? {
        "N" => 0.85,
        "R" => 0.62,
        _ => return None,
    };
    let cia = |k: &str| match get(k)? {
        "H" => Some(0.56),
        "L" => Some(0.22),
        "N" => Some(0.0),
        _ => None,
    };
    let (c, i, a) = (cia("C")?, cia("I")?, cia("A")?);
    let iss: f64 = 1.0 - (1.0 - c) * (1.0 - i) * (1.0 - a);
    let impact: f64 = if changed {
        7.52 * (iss - 0.029) - 3.25 * (iss - 0.02).powi(15)
    } else {
        6.42 * iss
    };
    if impact <= 0.0 {
        return Some(0.0);
    }
    let exploitability: f64 = 8.22 * av * ac * pr * ui;
    let raw: f64 = if changed {
        1.08 * (impact + exploitability)
    } else {
        impact + exploitability
    };
    Some(cvss_roundup(raw.min(10.0)))
}

/// CVSS 3.1 "Roundup": the smallest one-decimal number that is >= the input
/// (integer arithmetic, as the specification prescribes).
fn cvss_roundup(x: f64) -> f64 {
    let int_input = (x * 100_000.0).round() as i64;
    if int_input % 10_000 == 0 {
        int_input as f64 / 100_000.0
    } else {
        ((int_input / 10_000) as f64 + 1.0) / 10.0
    }
}

/// Qualitative rating of a CVSS score, in GitHub's vocabulary.
fn cvss_level(score: f64) -> &'static str {
    if score >= 9.0 {
        "CRITICAL"
    } else if score >= 7.0 {
        "HIGH"
    } else if score >= 4.0 {
        "MODERATE"
    } else if score > 0.0 {
        "LOW"
    } else {
        "NONE"
    }
}

/// Level of one OSV `severity[].score`: CVSS 3.x vectors and bare numbers
/// (0–10) are rated.
fn score_level(score: &str) -> Option<&'static str> {
    let score = cvss3_base_score(score).or_else(|| {
        score
            .trim()
            .parse::<f64>()
            .ok()
            .filter(|n| (0.0..=10.0).contains(n))
    })?;
    Some(cvss_level(score))
}

/// Severity from the advisory's stated level (`database_specific.severity`,
/// GitHub's review) and its `severity[].score` entries: a known stated level,
/// else a rated score, else a plain level among the scores, else whatever is
/// there.
fn pick_severity(stated: Option<String>, scores: &[&str]) -> Option<String> {
    if let Some(level) = stated.as_ref().filter(|s| severity_rank(s) > 0) {
        return Some(level.clone());
    }
    if let Some(level) = scores.iter().find_map(|s| score_level(s)) {
        return Some(level.to_string());
    }
    if let Some(level) = scores.iter().find(|s| severity_rank(s) > 0) {
        return Some(level.to_ascii_uppercase());
    }
    stated.or_else(|| {
        scores.first().map(|s| {
            if s.contains('/') {
                s.to_string()
            } else {
                s.to_ascii_uppercase()
            }
        })
    })
}

/// Turns the events of one OSV range into PEP 440 specifier sets (one per
/// affected interval) and the fixing versions. Events are evaluated in
/// version order like the OSV algorithm does: `introduced` opens an interval,
/// `fixed` (exclusive) or `last_affected` (inclusive) closes it. `limit`
/// events are ignored (they bound commit ranges), which can only widen the
/// result.
fn range_specifiers(events: &[Value]) -> (Vec<String>, Vec<String>) {
    let mut events: Vec<(&str, &str)> = events
        .iter()
        .filter_map(|e| {
            ["introduced", "fixed", "last_affected"]
                .into_iter()
                .find_map(|kind| {
                    Some((kind, str_at(e, kind).trim())).filter(|(_, v)| !v.is_empty())
                })
        })
        .collect();
    if events.iter().all(|(_, v)| parse_version(v).valid) {
        events.sort_by_cached_key(|(_, v)| parse_version(v));
    }
    let mut specs = Vec::new();
    let mut fixed = Vec::new();
    // Lower bound of the interval being built: `None` outside one, `Some("")` when unbounded (`introduced: 0`).
    let mut open: Option<String> = None;
    let mut started = false;
    for (kind, version) in events {
        if kind == "introduced" {
            if open.is_none() {
                open = Some(if version == "0" {
                    String::new()
                } else {
                    format!(">={version}")
                });
            }
            started = true;
            continue;
        }
        if kind == "fixed" {
            fixed.push(version.to_string());
        }
        let lower = match open.take() {
            Some(lower) => lower,
            // A leading `fixed`/`last_affected` without `introduced`: affected from the start.
            None if !started => String::new(),
            // Already outside an interval: nothing more becomes affected.
            None => continue,
        };
        started = true;
        let upper = if kind == "fixed" {
            format!("<{version}")
        } else {
            format!("<={version}")
        };
        specs.push(if lower.is_empty() {
            upper
        } else {
            format!("{lower},{upper}")
        });
    }
    if let Some(lower) = open {
        specs.push(if lower.is_empty() {
            ">=0".to_string()
        } else {
            lower
        });
    }
    (specs, fixed)
}

/// Specifier sets and fixed versions of one `affected[]` entry: its
/// ECOSYSTEM ranges (SEMVER as a fallback; GIT ranges hold commits, not
/// releases), plus `==v` for enumerated versions outside them.
fn entry_ranges(entry: &Value) -> (Vec<String>, Vec<String>) {
    let ranges = array_at(entry, "ranges");
    let of_type = |kind: &str| {
        ranges
            .iter()
            .filter(|r| str_at(r, "type").eq_ignore_ascii_case(kind))
            .collect::<Vec<_>>()
    };
    let mut chosen = of_type("ECOSYSTEM");
    if chosen.is_empty() {
        chosen = of_type("SEMVER");
    }
    let mut specs = Vec::new();
    let mut fixed = Vec::new();
    for range in chosen {
        let (s, f) = range_specifiers(array_at(range, "events"));
        specs.extend(s);
        fixed.extend(f);
    }
    let listed: Vec<String> = array_at(entry, "versions")
        .iter()
        .filter_map(Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .filter(|v| {
            let version = parse_version(v);
            !specs.iter().any(|s| specifier_matches(&version, s))
        })
        .map(|v| format!("=={v}"))
        .collect();
    specs.extend(listed);
    (specs, fixed)
}

/// The most useful link: a GitHub advisory page, then anything typed
/// `ADVISORY`, then web pages and reports, then fixes and the rest, package
/// pages last; `https` before `http`.
fn best_reference(doc: &Value) -> Option<String> {
    let rank = |kind: &str, url: &str| -> u8 {
        if url.contains("github.com/advisories/") || url.contains("/security/advisories/") {
            0
        } else if kind.eq_ignore_ascii_case("ADVISORY") {
            1
        } else if ["WEB", "ARTICLE", "REPORT"]
            .iter()
            .any(|k| kind.eq_ignore_ascii_case(k))
        {
            2
        } else if kind.eq_ignore_ascii_case("PACKAGE") {
            4
        } else {
            3
        }
    };
    array_at(doc, "references")
        .iter()
        .map(|r| (str_at(r, "type"), str_at(r, "url").trim()))
        .filter(|(_, url)| url.starts_with("https://") || url.starts_with("http://"))
        .min_by_key(|(kind, url)| (rank(kind, url), !url.starts_with("https://")))
        .map(|(_, url)| url.to_string())
}

fn parse_advisory(json: &str) -> Option<Advisory> {
    let doc: Value = serde_json::from_str(json).ok()?;
    let id = str_at(&doc, "id").trim();
    if id.is_empty() {
        return None;
    }
    let summary = match str_at(&doc, "summary").trim() {
        "" => summary_from_details(str_at(&doc, "details")),
        s => s.to_string(),
    };
    let stated = doc
        .get("database_specific")
        .map(|d| str_at(d, "severity").trim().to_ascii_uppercase())
        .filter(|s| !s.is_empty());
    let scores: Vec<&str> = array_at(&doc, "severity")
        .iter()
        .map(|s| str_at(s, "score").trim())
        .filter(|s| !s.is_empty())
        .collect();
    let mut packages: Vec<AffectedPackage> = Vec::new();
    for entry in array_at(&doc, "affected") {
        let Some(package) = entry.get("package") else {
            continue;
        };
        if !str_at(package, "ecosystem").eq_ignore_ascii_case("PyPI") {
            continue;
        }
        let name = normalize_name(str_at(package, "name"));
        let (ranges, fixed) = entry_ranges(entry);
        let index = match packages.iter().position(|p| p.name == name) {
            Some(i) => i,
            None => {
                packages.push(AffectedPackage {
                    name,
                    ..Default::default()
                });
                packages.len() - 1
            }
        };
        packages[index].ranges.extend(ranges);
        packages[index].fixed.extend(fixed);
    }
    for p in &mut packages {
        dedup_keep_order(&mut p.ranges);
        dedup_keep_order(&mut p.fixed);
    }
    let mut aliases: Vec<String> = array_at(&doc, "aliases")
        .iter()
        .filter_map(Value::as_str)
        .map(str::trim)
        .filter(|a| !a.is_empty() && *a != id)
        .map(str::to_string)
        .collect();
    dedup_keep_order(&mut aliases);
    Some(Advisory {
        id: id.to_string(),
        summary,
        severity: pick_severity(stated, &scores),
        reference: best_reference(&doc),
        aliases,
        withdrawn: !str_at(&doc, "withdrawn").trim().is_empty(),
        packages,
    })
}

/// Parses an OSV vulnerability document (`GET /v1/vulns/{id}`); `None` when
/// the JSON is malformed or has no id. Lenient: fields of unexpected shape
/// are skipped. The ranges are those of every PyPI package the advisory
/// lists (the checks keep only the queried package's).
pub fn parse_osv_vuln(json: &str) -> Option<Vulnerability> {
    parse_advisory(json).map(|a| a.to_vulnerability(None))
}

// ---------------------------------------------------------------------------
// Network
// ---------------------------------------------------------------------------

/// Agent for api.osv.dev with the IDE's proxy and certificate settings (`rustpy_net`).
fn agent() -> ureq::Agent {
    rustpy_net::agent_builder(OSV_QUERYBATCH_URL)
        .timeout_connect(TIMEOUT)
        .timeout(TIMEOUT)
        .max_idle_connections_per_host(DETAIL_THREADS)
        .user_agent(USER_AGENT)
        .build()
}

/// Reads a response body (capped at 64 MiB).
fn read_body(resp: ureq::Response) -> Result<String> {
    let mut text = String::new();
    resp.into_reader()
        .take(64 * 1024 * 1024)
        .read_to_string(&mut text)
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::InvalidData => {
                anyhow!("OSV sent a response that is not UTF-8 text")
            }
            _ => anyhow!("cannot reach OSV: reading the response failed: {e}"),
        })?;
    Ok(text)
}

/// Error for a failed request: `cannot reach OSV: …` for network problems
/// (no connection, timeouts), `OSV returned HTTP <code> …` otherwise.
fn describe(err: ureq::Error) -> anyhow::Error {
    match err {
        ureq::Error::Status(code, resp) => {
            let status = resp.status_text().to_string();
            let detail = resp.into_string().unwrap_or_default();
            match detail.trim() {
                "" => anyhow!("OSV returned HTTP {code} {status}"),
                d => anyhow!("OSV returned HTTP {code} {status}: {}", shorten(d, 200)),
            }
        }
        ureq::Error::Transport(t) => anyhow!(
            "cannot reach OSV: {}",
            rustpy_net::describe_transport_error(&t)
        ),
    }
}

/// `(normalized name, public version)` used as the OSV query, or `None` for
/// blank input (OSV would answer a version-less query with every advisory of
/// the package). Local version labels (`1.0+cpu`) are dropped: they denote
/// the same release.
fn query_key(name: &str, version: &str) -> Option<(String, String)> {
    let name = normalize_name(name);
    let version = version
        .trim()
        .split('+')
        .next()
        .unwrap_or("")
        .trim()
        .to_string();
    (!name.is_empty() && !version.is_empty()).then_some((name, version))
}

fn query_json(name: &str, version: &str, page_token: Option<&str>) -> Value {
    let mut query =
        serde_json::json!({"package": {"name": name, "ecosystem": "PyPI"}, "version": version});
    if let Some(token) = page_token {
        query["page_token"] = Value::from(token);
    }
    query
}

fn post_batch(agent: &ureq::Agent, queries: Vec<Value>) -> Result<Vec<BatchResult>> {
    let count = queries.len();
    let body = serde_json::json!({ "queries": queries }).to_string();
    let resp = agent
        .post(OSV_QUERYBATCH_URL)
        .set("Content-Type", "application/json")
        .set("Accept", "application/json")
        .send_string(&body)
        .map_err(describe)?;
    let results = parse_batch_results(&read_body(resp)?)?;
    if results.len() != count {
        bail!("OSV returned {} results for {count} queries", results.len());
    }
    Ok(results)
}

/// Advisory ids for every query key, in key order.
fn query_osv_batch(agent: &ureq::Agent, keys: &[(String, String)]) -> Result<Vec<Vec<String>>> {
    let mut ids: Vec<Vec<String>> = Vec::with_capacity(keys.len());
    for chunk in keys.chunks(BATCH_SIZE) {
        let base = ids.len();
        let mut pending: Vec<(usize, String)> = Vec::new();
        for (i, (found, token)) in post_batch(
            agent,
            chunk
                .iter()
                .map(|(name, version)| query_json(name, version, None))
                .collect(),
        )?
        .into_iter()
        .enumerate()
        {
            ids.push(found);
            if let Some(token) = token {
                pending.push((base + i, token));
            }
        }
        let mut pages = 1;
        while !pending.is_empty() {
            if pages == MAX_BATCH_PAGES {
                log::warn!(
                    "OSV has more results for {} queries after {pages} pages; ignoring them",
                    pending.len()
                );
                break;
            }
            pages += 1;
            let queries = pending
                .iter()
                .map(|(k, token)| query_json(&keys[*k].0, &keys[*k].1, Some(token.as_str())))
                .collect();
            let results = post_batch(agent, queries)?;
            let mut next = Vec::new();
            for ((k, _), (found, token)) in pending.iter().zip(results) {
                ids[*k].extend(found);
                if let Some(token) = token {
                    next.push((*k, token));
                }
            }
            pending = next;
        }
    }
    for list in &mut ids {
        dedup_keep_order(list);
    }
    Ok(ids)
}

/// `GET /v1/vulns/{id}`; `Ok(None)` for unknown ids or unusable documents.
fn fetch_advisory(agent: &ureq::Agent, id: &str) -> Result<Option<Advisory>> {
    let valid = id.starts_with(|c: char| c.is_ascii_alphanumeric())
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':'));
    if !valid {
        bail!("`{id}` is not an OSV id");
    }
    match agent
        .get(&format!("{OSV_VULN_URL}/{id}"))
        .set("Accept", "application/json")
        .call()
    {
        Ok(resp) => Ok(parse_advisory(&read_body(resp)?)),
        Err(ureq::Error::Status(404, _)) => Ok(None),
        Err(e) => Err(describe(e)),
    }
}

/// Worker loop: fetches ids from the shared queue until it is empty.
fn drain_queue(
    agent: &ureq::Agent,
    queue: &Mutex<std::vec::IntoIter<String>>,
) -> Vec<(String, Advisory)> {
    let mut fetched = Vec::new();
    loop {
        // Own statement, so the lock is released before fetching.
        let next = queue.lock().unwrap_or_else(PoisonError::into_inner).next();
        let Some(id) = next else { break };
        match fetch_advisory(agent, &id) {
            Ok(Some(advisory)) => fetched.push((id, advisory)),
            Ok(None) => log::warn!("OSV has no usable details for {id}"),
            Err(e) => log::warn!("cannot fetch OSV details for {id}: {e:#}"),
        }
    }
    fetched
}

/// Fetches the given advisories on up to [`DETAIL_THREADS`] threads; failed
/// lookups are logged and left out.
fn fetch_concurrently(agent: &ureq::Agent, ids: Vec<String>) -> Vec<(String, Advisory)> {
    let workers = ids.len().min(DETAIL_THREADS);
    let queue = Mutex::new(ids.into_iter());
    std::thread::scope(|scope| {
        let mut handles = Vec::with_capacity(workers);
        for _ in 0..workers {
            handles.push(scope.spawn(|| drain_queue(agent, &queue)));
        }
        // A panicked worker (there should be none) only loses its own lookups.
        handles
            .into_iter()
            .flat_map(|h| h.join().unwrap_or_default())
            .collect()
    })
}

/// Picks up to `budget` ids from the per-package lists in turns (the first
/// wanted id of every package, then the second, …), so that one package
/// with many advisories cannot use up the whole budget.
fn round_robin(
    lists: &[Vec<String>],
    budget: usize,
    mut want: impl FnMut(&str) -> bool,
) -> Vec<String> {
    let mut picked = Vec::new();
    let mut cursors = vec![0; lists.len()];
    while picked.len() < budget {
        let mut progressed = false;
        for (list, cursor) in lists.iter().zip(cursors.iter_mut()) {
            if picked.len() == budget {
                break;
            }
            while let Some(id) = list.get(*cursor) {
                *cursor += 1;
                if want(id.as_str()) {
                    picked.push(id.clone());
                    progressed = true;
                    break;
                }
            }
        }
        if !progressed {
            break;
        }
    }
    picked
}

/// Fetches advisory details within [`MAX_DETAIL_LOOKUPS`]. `GHSA-*` entries
/// go first (they carry a summary and a reviewed severity, and name their
/// `PYSEC-*` twin as an alias), except for packages without any; the second
/// pass only looks up what the first one's aliases do not already cover.
fn fetch_details(agent: &ureq::Agent, ids_per_key: &[Vec<String>]) -> HashMap<String, Advisory> {
    let mut requested: HashSet<String> = HashSet::new();
    let primary: Vec<Vec<String>> = ids_per_key
        .iter()
        .map(|ids| {
            let github: Vec<String> = ids
                .iter()
                .filter(|id| id.starts_with("GHSA-"))
                .cloned()
                .collect();
            if github.is_empty() {
                ids.clone()
            } else {
                github
            }
        })
        .collect();
    let first = round_robin(&primary, MAX_DETAIL_LOOKUPS, |id| {
        requested.insert(id.to_string())
    });
    let mut details: HashMap<String, Advisory> =
        fetch_concurrently(agent, first).into_iter().collect();
    let budget = MAX_DETAIL_LOOKUPS.saturating_sub(requested.len());
    let covered: HashSet<String> = details
        .values()
        .flat_map(|a| a.aliases.iter().cloned())
        .collect();
    let second = round_robin(ids_per_key, budget, |id| {
        !covered.contains(id) && requested.insert(id.to_string())
    });
    details.extend(fetch_concurrently(agent, second));
    details
}

/// Sort key putting the most useful of two twins first: details, a
/// severity, a summary, GitHub's entry, then the id.
fn informativeness(a: &Advisory) -> (bool, bool, bool, bool, &str) {
    (
        !a.has_details(),
        a.severity.is_none(),
        a.summary.is_empty(),
        !a.id.starts_with("GHSA-"),
        a.id.as_str(),
    )
}

/// Drops advisories that duplicate a more informative one (their id among
/// its aliases, or a shared alias such as a CVE id).
fn collapse_aliases(mut advisories: Vec<Advisory>) -> Vec<Advisory> {
    advisories.sort_by(|a, b| informativeness(a).cmp(&informativeness(b)));
    let mut covered: HashSet<String> = HashSet::new();
    let mut kept = Vec::new();
    for a in advisories {
        if covered.contains(&a.id) || a.aliases.iter().any(|x| covered.contains(x)) {
            continue;
        }
        covered.insert(a.id.clone());
        covered.extend(a.aliases.iter().cloned());
        kept.push(a);
    }
    kept
}

/// The report for one package (normalized name): its advisories (id-only
/// stubs where details are missing) without withdrawn ones, twins
/// collapsed, ranges of that package, sorted by severity (highest first),
/// advisories with details before id-only ones, then by id.
fn vulnerabilities_for(
    package: &str,
    ids: &[String],
    details: &HashMap<String, Advisory>,
) -> Vec<Vulnerability> {
    let advisories = ids
        .iter()
        .map(|id| {
            details
                .get(id)
                .cloned()
                .unwrap_or_else(|| Advisory::stub(id))
        })
        .filter(|a| !a.withdrawn)
        .collect();
    let mut kept = collapse_aliases(advisories);
    let rank = |a: &Advisory| severity_rank(a.severity.as_deref().unwrap_or(""));
    kept.sort_by(|a, b| {
        rank(b)
            .cmp(&rank(a))
            .then_with(|| b.has_details().cmp(&a.has_details()))
            .then_with(|| a.id.cmp(&b.id))
    });
    kept.iter()
        .map(|a| a.to_vulnerability(Some(package)))
        .collect()
}

/// Runs the check, returning `(input index, vulnerabilities)` for the inputs
/// with at least one known vulnerability, in input order. Identical queries
/// are sent once.
fn check_indexed(packages: &[(String, String)]) -> Result<Vec<(usize, Vec<Vulnerability>)>> {
    let mut keys: Vec<(String, String)> = Vec::new();
    let mut slots: HashMap<(String, String), usize> = HashMap::new();
    let key_of_input: Vec<Option<usize>> = packages
        .iter()
        .map(|(name, version)| {
            let key = query_key(name, version)?;
            Some(*slots.entry(key.clone()).or_insert_with(|| {
                keys.push(key);
                keys.len() - 1
            }))
        })
        .collect();
    if keys.is_empty() {
        return Ok(Vec::new());
    }
    let agent = agent();
    let ids_per_key = query_osv_batch(&agent, &keys)?;
    let details = fetch_details(&agent, &ids_per_key);
    let reports: Vec<Vec<Vulnerability>> = keys
        .iter()
        .zip(&ids_per_key)
        .map(|((name, _), ids)| vulnerabilities_for(name, ids, &details))
        .collect();
    Ok(key_of_input
        .iter()
        .enumerate()
        .filter_map(|(i, slot)| {
            let vulns = &reports[(*slot)?];
            (!vulns.is_empty()).then(|| (i, vulns.clone()))
        })
        .collect())
}

/// Looks `(name, version)` pairs up in OSV. Returns, in input order, the
/// packages with at least one known vulnerability (name as given) with their
/// advisories, most severe first. Blank names or versions are skipped; an
/// empty input makes no request.
///
/// `Err` for network problems (the message starts with `cannot reach OSV`
/// when the service is unreachable or timed out), HTTP errors (`OSV returned
/// HTTP <code>`) and malformed batch responses; failed or capped detail
/// lookups degrade to id-only entries instead.
pub fn check_vulnerabilities(
    packages: &[(String, String)],
) -> Result<Vec<(String, Vec<Vulnerability>)>> {
    Ok(check_indexed(packages)?
        .into_iter()
        .map(|(i, vulns)| (packages[i].0.clone(), vulns))
        .collect())
}

/// Checks `requirements.txt` entries: a pinned requirement (an `==`/`===`
/// clause with one exact version, see [`pinned_version`]) is checked at its
/// pin, any other one at the installed version of the package (skipped when
/// it is not installed). Environment markers are not evaluated. Returns
/// `(requirement, checked version, vulnerabilities)` for the requirements
/// with known vulnerabilities, in input order; errors as for
/// [`check_vulnerabilities`].
pub fn check_requirements(
    reqs: &[Requirement],
    installed: &[InstalledPackage],
) -> Result<Vec<(Requirement, String, Vec<Vulnerability>)>> {
    let targets = requirement_versions(reqs, installed);
    let packages: Vec<(String, String)> = targets
        .iter()
        .map(|(req, version)| (req.name.clone(), version.clone()))
        .collect();
    Ok(check_indexed(&packages)?
        .into_iter()
        .map(|(i, vulns)| {
            let (req, version) = &targets[i];
            (req.clone(), version.clone(), vulns)
        })
        .collect())
}

/// The exact version a requirement pins: the operand of an `==` or `===`
/// clause without a `.*` wildcard (other clauses beside it cannot widen a
/// pin). `None` for unpinned requirements.
pub fn pinned_version(req: &Requirement) -> Option<&str> {
    req.specifier.as_deref()?.split(',').find_map(|clause| {
        let clause = clause.trim();
        let version = clause
            .strip_prefix("===")
            .or_else(|| clause.strip_prefix("=="))?
            .trim();
        (!version.is_empty() && !version.ends_with(".*")).then_some(version)
    })
}

/// The version to check for each requirement: the pin, else the installed version.
fn requirement_versions(
    reqs: &[Requirement],
    installed: &[InstalledPackage],
) -> Vec<(Requirement, String)> {
    reqs.iter()
        .filter_map(|req| {
            let version = match pinned_version(req) {
                Some(v) => v.to_string(),
                None => {
                    let key = normalize_name(&req.name);
                    installed
                        .iter()
                        .find(|p| normalize_name(&p.name) == key)?
                        .version
                        .clone()
                }
            };
            Some((req.clone(), version))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::requirements::parse_requirements;

    // Real OSV API responses (fetched 2026-10-09), verbatim: the batch answer
    // for requests 2.19.0 plus a package that does not exist, and five of the
    // ten advisories it lists.
    const BATCH_REQUESTS: &str =
        include_str!("../tests/fixtures/osv/querybatch-requests-2.19.0.json");
    const GHSA_J8R2: &str = include_str!("../tests/fixtures/osv/GHSA-j8r2-6x86-q33q.json");
    const GHSA_X84V: &str = include_str!("../tests/fixtures/osv/GHSA-x84v-xcm2-53pg.json");
    const PYSEC_2018_28: &str = include_str!("../tests/fixtures/osv/PYSEC-2018-28.json");
    const PYSEC_2023_74: &str = include_str!("../tests/fixtures/osv/PYSEC-2023-74.json");
    const PYSEC_2026_2275: &str = include_str!("../tests/fixtures/osv/PYSEC-2026-2275.json");

    /// Synthetic: several packages and ranges (events out of order, a
    /// redundant `introduced`, a `fixed` outside any interval, an open end),
    /// an enumerated version outside the ranges, a non-PyPI entry, a GIT
    /// range, a CVSS vector but no stated level.
    const MULTI_JSON: &str = r#"{
        "id": "OSV-TEST-1",
        "summary": "  Padded summary  ",
        "aliases": ["CVE-2000-0001", "OSV-TEST-1", "CVE-2000-0001"],
        "severity": [{"type": "CVSS_V3", "score": "CVSS:3.1/AV:N/AC:L/PR:N/UI:N/S:U/C:H/I:H/A:H"}],
        "references": [{"type": "PACKAGE", "url": "https://pypi.org/project/demo"}, {"type": "REPORT", "url": "http://example.invalid/report"}, {"type": "WEB", "url": "https://example.invalid/web"}],
        "affected": [
            {"package": {"name": "left-pad", "ecosystem": "npm"}, "ranges": [{"type": "SEMVER", "events": [{"introduced": "0"}, {"fixed": "9.9.9"}]}]},
            {"package": {"name": "Demo_Pkg", "ecosystem": "PyPI"}, "ranges": [
                {"type": "GIT", "repo": "https://example.invalid/demo", "events": [{"introduced": "0"}, {"fixed": "abcdef"}]},
                {"type": "ECOSYSTEM", "events": [{"introduced": "3.0"}, {"last_affected": "3.0.1"}, {"introduced": "0"}, {"fixed": "1.2"}, {"introduced": "2.0"}, {"introduced": "2.0.5"}, {"fixed": "2.1"}, {"fixed": "2.2"}, {"introduced": "4.0"}]}
            ], "versions": ["1.0", "2.0.1", "2.5"]},
            {"package": {"name": "demo-pkg", "ecosystem": "PyPI"}, "ranges": [{"type": "ECOSYSTEM", "events": [{"introduced": "0"}, {"fixed": "1.2"}]}]},
            {"package": {"name": "demo-plugin", "ecosystem": "PyPI"}, "ranges": [{"type": "ECOSYSTEM", "events": [{"introduced": "0.5"}, {"fixed": "0.9"}]}]}
        ]
    }"#;

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    fn details_of(docs: &[&str]) -> HashMap<String, Advisory> {
        docs.iter()
            .map(|json| {
                let a = parse_advisory(json).expect("fixture parses");
                (a.id.clone(), a)
            })
            .collect()
    }

    #[test]
    fn parses_real_batch_response() {
        let ids = parse_osv_batch(BATCH_REQUESTS);
        assert_eq!(ids.len(), 2);
        assert_eq!(
            ids[0],
            [
                "GHSA-9hjg-9r4m-mvj7",
                "GHSA-9wx4-h78v-vm56",
                "GHSA-gc5v-m9x4-r6x2",
                "GHSA-j8r2-6x86-q33q",
                "GHSA-x84v-xcm2-53pg",
                "PYSEC-2018-28",
                "PYSEC-2023-74",
                "PYSEC-2026-1872",
                "PYSEC-2026-1873",
                "PYSEC-2026-2275"
            ]
        );
        assert!(ids[1].is_empty());
        assert!(parse_batch_results(BATCH_REQUESTS)
            .unwrap()
            .iter()
            .all(|(_, token)| token.is_none()));
    }

    #[test]
    fn parses_batch_edge_cases() {
        let json = r#"{"results":[{"vulns":[{"id":"GHSA-1"},{"id":"PYSEC-1"},{"id":"GHSA-1"},{"modified":"x"},{"id":" "}]},{},{"vulns":null},{"vulns":[{"id":"OSV-1"}],"next_page_token":"tok"}]}"#;
        assert_eq!(
            parse_osv_batch(json),
            vec![
                strings(&["GHSA-1", "PYSEC-1"]),
                vec![],
                vec![],
                strings(&["OSV-1"])
            ]
        );
        let results = parse_batch_results(json).unwrap();
        assert_eq!(results[0].1, None);
        assert_eq!(results[3].1.as_deref(), Some("tok"));
        assert!(parse_osv_batch(r#"{"results":[]}"#).is_empty());
        assert!(parse_osv_batch("not json").is_empty());
        assert!(parse_osv_batch(r#"{"foo": 1}"#).is_empty());
        assert!(parse_batch_results(r#"{"foo": 1}"#).is_err());
    }

    #[test]
    fn parses_real_github_advisory() {
        let v = parse_osv_vuln(GHSA_J8R2).unwrap();
        assert_eq!(v.id, "GHSA-j8r2-6x86-q33q");
        assert_eq!(
            v.summary,
            "Unintended leak of Proxy-Authorization header in requests"
        );
        assert_eq!(v.severity.as_deref(), Some("MODERATE")); // stated by GitHub
        assert_eq!(v.affected_versions, [">=2.3.0,<2.31.0"]); // the 66 enumerated versions all lie inside
        assert_eq!(v.fixed_versions, ["2.31.0"]);
        assert_eq!(
            v.reference.as_deref(),
            Some("https://github.com/psf/requests/security/advisories/GHSA-j8r2-6x86-q33q")
        );
        assert_eq!(
            v.osv_url(),
            "https://osv.dev/vulnerability/GHSA-j8r2-6x86-q33q"
        );
        for (version, affected) in [
            ("2.19.0", true),
            ("2.3.0", true),
            ("2.30.0", true),
            ("2.2.1", false),
            ("2.31.0", false),
            ("2.32.4", false),
        ] {
            assert_eq!(v.affects(version), affected, "{version}");
        }
        let advisory = parse_advisory(GHSA_J8R2).unwrap();
        assert_eq!(advisory.aliases, ["CVE-2023-32681", "PYSEC-2023-74"]);
        assert!(!advisory.withdrawn);
        assert_eq!(advisory.packages.len(), 1);
        assert_eq!(advisory.packages[0].name, "requests");

        let v = parse_osv_vuln(GHSA_X84V).unwrap();
        assert_eq!(
            v.summary,
            "Insufficiently Protected Credentials in Requests"
        );
        assert_eq!(v.severity.as_deref(), Some("HIGH"));
        assert_eq!(v.affected_versions, ["<2.20.0"]);
        assert_eq!(v.fixed_versions, ["2.20.0"]);
        // No GitHub advisory page among the references: the NVD entry (typed ADVISORY) wins over WEB links.
        assert_eq!(
            v.reference.as_deref(),
            Some("https://nvd.nist.gov/vuln/detail/CVE-2018-18074")
        );
    }

    #[test]
    fn parses_real_pypa_advisories_without_summary() {
        let v = parse_osv_vuln(PYSEC_2018_28).unwrap();
        assert_eq!(v.id, "PYSEC-2018-28");
        assert_eq!(v.summary, "The Requests package before 2.20.0 for Python sends an HTTP Authorization header to an http URI upon receiving a same-hostname https-to-http redirect, which makes it easier for remote attackers to…");
        assert_eq!(v.severity, None);
        // Only the ECOSYSTEM range counts; the GIT range's commit hashes are not versions.
        assert_eq!(v.affected_versions, ["<2.20.0"]);
        assert_eq!(v.fixed_versions, ["2.20.0"]);
        // The GitHub advisory page beats the other ADVISORY/WEB/FIX references.
        assert_eq!(
            v.reference.as_deref(),
            Some("https://github.com/advisories/GHSA-x84v-xcm2-53pg")
        );
        assert!(v.affects("2.19.0") && !v.affects("2.20.0"));

        let v = parse_osv_vuln(PYSEC_2023_74).unwrap();
        assert_eq!(v.summary, "Requests is a HTTP library. Since Requests 2.3.0, Requests has been leaking Proxy-Authorization headers to destination servers when redirected to an HTTPS endpoint. This is a product of how we use…");
        assert_eq!(v.severity, None);
        assert_eq!(v.affected_versions, [">=2.3.0,<2.31.0"]);
        assert_eq!(
            v.reference.as_deref(),
            Some("https://github.com/psf/requests/security/advisories/GHSA-j8r2-6x86-q33q")
        );

        // No stated level, but a CVSS 3.1 vector (AV:L/AC:L/PR:L/UI:N/S:U/C:N/I:H/A:N scores 5.5).
        let v = parse_osv_vuln(PYSEC_2026_2275).unwrap();
        assert_eq!(v.severity.as_deref(), Some("MODERATE"));
        assert!(v.summary.starts_with("Requests is a HTTP library. Prior to version 2.33.0, the `requests.utils.extract_zipped_paths()` utility"), "{}", v.summary);
        assert_eq!(v.affected_versions, ["<2.33.0"]);
        assert_eq!(v.fixed_versions, ["2.33.0"]);
        assert_eq!(
            v.reference.as_deref(),
            Some("https://github.com/psf/requests/security/advisories/GHSA-gc5v-m9x4-r6x2")
        );
    }

    #[test]
    fn parses_multiple_packages_and_ranges() {
        let v = parse_osv_vuln(MULTI_JSON).unwrap();
        assert_eq!(v.summary, "Padded summary");
        assert_eq!(v.severity.as_deref(), Some("CRITICAL")); // CVSS 9.8
        assert_eq!(v.reference.as_deref(), Some("https://example.invalid/web"));
        // Without a package to focus on, the ranges of every PyPI package (the npm entry is ignored).
        assert_eq!(
            v.affected_versions,
            [
                "<1.2",
                ">=2.0,<2.1",
                ">=3.0,<=3.0.1",
                ">=4.0",
                "==2.5",
                ">=0.5,<0.9"
            ]
        );
        assert_eq!(v.fixed_versions, ["0.9", "1.2", "2.1", "2.2"]);
        let advisory = parse_advisory(MULTI_JSON).unwrap();
        assert_eq!(advisory.aliases, ["CVE-2000-0001"]);
        // `Demo_Pkg` and `demo-pkg` are one package.
        let names: Vec<&str> = advisory.packages.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["demo-pkg", "demo-plugin"]);
        let pkg = advisory.to_vulnerability(Some("demo-pkg"));
        assert_eq!(
            pkg.affected_versions,
            ["<1.2", ">=2.0,<2.1", ">=3.0,<=3.0.1", ">=4.0", "==2.5"]
        );
        assert_eq!(pkg.fixed_versions, ["1.2", "2.1", "2.2"]);
        for (version, affected) in [
            ("1.1", true),
            ("1.2", false),
            ("1.5", false),
            ("2.0", true),
            ("2.0.5", true),
            ("2.1", false),
            ("2.5", true),
            ("2.6", false),
            ("3.0.1", true),
            ("3.1", false),
            ("4.5", true),
        ] {
            assert_eq!(pkg.affects(version), affected, "{version}");
        }
        let plugin = advisory.to_vulnerability(Some("demo-plugin"));
        assert_eq!(plugin.affected_versions, [">=0.5,<0.9"]);
        assert_eq!(plugin.fixed_versions, ["0.9"]);
        assert!(plugin.affects("0.5") && !plugin.affects("1.1"));
        // A package the advisory does not list sees all ranges.
        assert_eq!(
            advisory.to_vulnerability(Some("other")).affected_versions,
            v.affected_versions
        );
    }

    #[test]
    fn converts_range_events() {
        let events = |json: &str| -> (Vec<String>, Vec<String>) {
            range_specifiers(
                serde_json::from_str::<Value>(json)
                    .unwrap()
                    .as_array()
                    .unwrap(),
            )
        };
        assert_eq!(
            events(r#"[{"introduced": "0"}]"#),
            (strings(&[">=0"]), vec![])
        );
        assert_eq!(
            events(r#"[{"introduced": "1.0"}, {"limit": "2.0"}]"#),
            (strings(&[">=1.0"]), vec![])
        );
        assert_eq!(
            events(r#"[{"fixed": "1.0"}]"#),
            (strings(&["<1.0"]), strings(&["1.0"]))
        );
        assert_eq!(
            events(r#"[{"introduced": "1.0"}, {"last_affected": "1.4"}]"#),
            (strings(&[">=1.0,<=1.4"]), vec![])
        );
        assert_eq!(
            events(r#"[{"introduced": " "}, {"fixed": 7}, {}]"#),
            (vec![], vec![])
        );
        assert_eq!(events("[]"), (vec![], vec![]));
    }

    #[test]
    fn parses_degenerate_documents() {
        assert!(parse_osv_vuln("").is_none());
        assert!(parse_osv_vuln("{}").is_none());
        assert!(parse_osv_vuln("[]").is_none());
        assert!(parse_osv_vuln(r#"{"id": "  "}"#).is_none());
        let bare = parse_osv_vuln(r#"{"id": "OSV-BARE", "summary": 3, "severity": "oops", "affected": 7, "references": {}}"#).unwrap();
        assert_eq!(
            bare,
            Vulnerability {
                id: "OSV-BARE".into(),
                ..Default::default()
            }
        );
        assert!(bare.affects("1.0")); // unknown ranges: assume affected
                                      // Enumerated versions only, a stated level in odd case, a non-http reference.
        let listed = parse_osv_vuln(r#"{"id": "OSV-LIST", "database_specific": {"severity": "high"}, "references": [{"type": "WEB", "url": "ftp://x"}], "affected": [{"package": {"ecosystem": "PyPI"}, "versions": ["1.0", "1.1", " ", 2]}]}"#).unwrap();
        assert_eq!(listed.severity.as_deref(), Some("HIGH"));
        assert_eq!(listed.affected_versions, ["==1.0", "==1.1"]);
        assert!(listed.affects("1.0") && !listed.affects("1.2"));
        assert_eq!(listed.reference, None);
        // A CVSS 4.0 vector cannot be scored here and is passed through.
        let v4 = parse_osv_vuln(r#"{"id": "OSV-V4", "severity": [{"type": "CVSS_V4", "score": "CVSS:4.0/AV:N/AC:L/AT:N/PR:N/UI:N/VC:H/VI:H/VA:H/SC:N/SI:N/SA:N"}]}"#).unwrap();
        assert_eq!(
            v4.severity.as_deref(),
            Some("CVSS:4.0/AV:N/AC:L/AT:N/PR:N/UI:N/VC:H/VI:H/VA:H/SC:N/SI:N/SA:N")
        );
        assert_eq!(severity_rank(v4.severity.as_deref().unwrap()), 0);
        // Plain levels are upper-cased, and preferred over an unscorable vector.
        let plain = parse_osv_vuln(r#"{"id": "OSV-PLAIN", "severity": [{"type": "CVSS_V4", "score": "CVSS:4.0/AV:N"}, {"type": "Ubuntu", "score": "medium"}]}"#).unwrap();
        assert_eq!(plain.severity.as_deref(), Some("MEDIUM"));
        // An unknown stated level falls back to the CVSS vector; bare numbers are rated.
        let unknown = parse_osv_vuln(r#"{"id": "OSV-UNK", "database_specific": {"severity": "UNKNOWN"}, "severity": [{"type": "CVSS_V3", "score": "CVSS:3.1/AV:N/AC:L/PR:N/UI:N/S:U/C:H/I:N/A:N"}]}"#).unwrap();
        assert_eq!(unknown.severity.as_deref(), Some("HIGH"));
        let numeric =
            parse_osv_vuln(r#"{"id": "OSV-NUM", "severity": [{"type": "X", "score": "3.1"}]}"#)
                .unwrap();
        assert_eq!(numeric.severity.as_deref(), Some("LOW"));
        let only_unknown =
            parse_osv_vuln(r#"{"id": "OSV-UNK2", "database_specific": {"severity": "unknown"}}"#)
                .unwrap();
        assert_eq!(only_unknown.severity.as_deref(), Some("UNKNOWN"));
        // Withdrawn advisories still parse; the checks drop them.
        assert!(
            parse_advisory(r#"{"id": "OSV-W", "withdrawn": "2024-01-01T00:00:00Z"}"#)
                .unwrap()
                .withdrawn
        );
        assert!(
            parse_osv_vuln(r#"{"id": "OSV-W", "withdrawn": "2024-01-01T00:00:00Z"}"#).is_some()
        );
    }

    #[test]
    fn scores_cvss3_vectors_like_nvd() {
        let cases = [
            (
                "CVSS:3.1/AV:L/AC:H/PR:H/UI:R/S:U/C:H/I:H/A:N",
                5.6,
                "MODERATE",
            ), // CVE-2024-35195
            (
                "CVSS:3.1/AV:N/AC:H/PR:N/UI:R/S:C/C:H/I:N/A:N",
                6.1,
                "MODERATE",
            ), // CVE-2023-32681
            (
                "CVSS:3.1/AV:N/AC:H/PR:N/UI:R/S:U/C:H/I:N/A:N",
                5.3,
                "MODERATE",
            ), // CVE-2024-47081
            ("CVSS:3.1/AV:N/AC:L/PR:N/UI:N/S:U/C:H/I:N/A:N", 7.5, "HIGH"), // CVE-2018-18074
            (
                "CVSS:3.1/AV:N/AC:L/PR:N/UI:N/S:U/C:H/I:H/A:H",
                9.8,
                "CRITICAL",
            ),
            (
                "CVSS:3.1/AV:N/AC:L/PR:L/UI:R/S:C/C:L/I:L/A:N",
                5.4,
                "MODERATE",
            ), // classic XSS
            ("CVSS:3.0/AV:P/AC:H/PR:H/UI:R/S:U/C:L/I:N/A:N", 1.6, "LOW"),
            (
                "CVSS:3.1/AV:N/AC:L/PR:N/UI:N/S:U/C:H/I:H/A:H/",
                9.8,
                "CRITICAL",
            ), // trailing slash
            (
                "CVSS:3.1/AV:N/AC:L/PR:N/UI:N/S:U/C:N/I:N/A:N/E:P/RL:O/RC:C",
                0.0,
                "NONE",
            ),
        ];
        for (vector, score, level) in cases {
            let got = cvss3_base_score(vector).unwrap();
            assert!((got - score).abs() < 1e-9, "{vector}: {got} != {score}");
            assert_eq!(cvss_level(got), level, "{vector}");
        }
        assert_eq!(
            cvss3_base_score("CVSS:4.0/AV:N/AC:L/AT:N/PR:N/UI:N/VC:H/VI:H/VA:H/SC:N/SI:N/SA:N"),
            None
        );
        assert_eq!(
            cvss3_base_score("CVSS:3.1/AV:X/AC:L/PR:N/UI:N/S:U/C:H/I:H/A:H"),
            None
        );
        assert_eq!(cvss3_base_score("CVSS:3.1/AV:N/AC:L"), None);
        assert_eq!(
            cvss3_base_score("CVSS:3.1/AV:N/AC:L/PR:N/UI:N/S:U/C:H/I:H/A"),
            None
        );
        assert_eq!(cvss3_base_score(""), None);
        assert_eq!(cvss_roundup(4.02), 4.1);
        assert_eq!(cvss_roundup(4.0), 4.0);
        assert_eq!(cvss_roundup(4.000_000_000_000_001), 4.0);
        assert_eq!(score_level("NaN"), None);
        assert_eq!(score_level("11"), None);
        assert_eq!(score_level("9.0"), Some("CRITICAL"));
    }

    #[test]
    fn ranks_severities() {
        assert_eq!(severity_rank("CRITICAL"), 4);
        assert_eq!(severity_rank("High"), 3);
        assert_eq!(severity_rank("moderate"), 2);
        assert_eq!(severity_rank("MEDIUM"), 2);
        assert_eq!(severity_rank(" low "), 1);
        assert_eq!(severity_rank("NONE"), 0);
        assert_eq!(severity_rank(""), 0);
        assert_eq!(
            severity_rank("CVSS:3.1/AV:N/AC:L/PR:N/UI:N/S:U/C:H/I:H/A:H"),
            0
        );
    }

    #[test]
    fn summarizes_details() {
        let details = format!("## Impact\n\n{}\n\nmore", "lorem ipsum ".repeat(30));
        let s = summary_from_details(&details);
        assert!(s.starts_with("lorem ipsum lorem"), "{s}");
        assert!(s.ends_with("m…"), "{s}");
        assert!(s.chars().count() <= 201);
        assert_eq!(summary_from_details("# Heading only"), "");
        assert_eq!(summary_from_details(""), "");
        // A wrapped paragraph is joined; the next paragraph or heading ends it.
        assert_eq!(
            summary_from_details("\n  short text  \nsecond line\n\nnext paragraph"),
            "short text second line"
        );
        assert_eq!(
            summary_from_details("### Impact\nfirst\n### Patches\nsecond"),
            "first"
        );
        assert_eq!(shorten("abc", 3), "abc");
        assert_eq!(shorten("abcdef", 3), "abc…");
        assert_eq!(shorten("ab cd ef", 6), "ab cd…");
        assert_eq!(shorten("ab cd ef", 5), "ab cd…"); // the cut falls on a word boundary
        assert_eq!(shorten("ab cdef", 5), "ab…");
    }

    #[test]
    fn finds_pinned_versions() {
        let reqs = parse_requirements("a==1.2\nb===1.2.3\nc==1.*\nd>=1\ne>=1.0,==1.2\nf\ng~=1.0\nh!=1.0\ni == 2.0 ; python_version < \"3.8\"\n");
        let pins: Vec<Option<&str>> = reqs.iter().map(pinned_version).collect();
        assert_eq!(
            pins,
            [
                Some("1.2"),
                Some("1.2.3"),
                None,
                None,
                Some("1.2"),
                None,
                None,
                None,
                Some("2.0")
            ]
        );
    }

    #[test]
    fn maps_requirements_to_versions() {
        let reqs = parse_requirements("requests==2.19.0\nDjango>=3\nzope.interface\nmissing-pkg\nurllib3==1.24 ; python_version < \"3.8\"\n");
        let installed = vec![
            InstalledPackage {
                name: "requests".into(),
                version: "2.31.0".into(),
                location: None,
                editable: false,
                summary: None,
            },
            InstalledPackage {
                name: "django".into(),
                version: "4.2".into(),
                location: None,
                editable: false,
                summary: None,
            },
            InstalledPackage {
                name: "Zope_Interface".into(),
                version: "6.0".into(),
                location: None,
                editable: false,
                summary: None,
            },
        ];
        let targets = requirement_versions(&reqs, &installed);
        let got: Vec<(&str, &str, usize)> = targets
            .iter()
            .map(|(r, v)| (r.name.as_str(), v.as_str(), r.line))
            .collect();
        // The pin wins over the installed version; unpinned requirements use the installed version; missing packages are skipped.
        assert_eq!(
            got,
            [
                ("requests", "2.19.0", 1),
                ("Django", "4.2", 2),
                ("zope.interface", "6.0", 3),
                ("urllib3", "1.24", 5)
            ]
        );
    }

    #[test]
    fn builds_queries_without_touching_the_network_for_empty_input() {
        assert_eq!(
            query_key("Requests", "2.19.0+cpu"),
            Some(("requests".to_string(), "2.19.0".to_string()))
        );
        assert_eq!(
            query_key("Zope.Interface", " 6.0 "),
            Some(("zope-interface".to_string(), "6.0".to_string()))
        );
        assert_eq!(query_key("", "1.0"), None);
        assert_eq!(query_key("x", " "), None);
        assert_eq!(query_key("x", "+local"), None);
        assert_eq!(
            query_json("requests", "2.19.0", None),
            serde_json::json!({"package": {"name": "requests", "ecosystem": "PyPI"}, "version": "2.19.0"})
        );
        assert_eq!(
            query_json("requests", "2.19.0", Some("tok")),
            serde_json::json!({"package": {"name": "requests", "ecosystem": "PyPI"}, "version": "2.19.0", "page_token": "tok"})
        );
        // No queries, no request.
        assert!(check_vulnerabilities(&[]).unwrap().is_empty());
        assert!(check_vulnerabilities(&[
            (" ".to_string(), "1.0".to_string()),
            ("x".to_string(), "".to_string())
        ])
        .unwrap()
        .is_empty());
        assert!(check_requirements(&parse_requirements("flask>=2\n"), &[])
            .unwrap()
            .is_empty());
    }

    #[test]
    fn shares_the_lookup_budget_in_turns() {
        let lists = vec![
            strings(&["A1", "A2", "A3", "A4"]),
            vec![],
            strings(&["B1", "A1", "B2"]),
            strings(&["C1"]),
        ];
        let mut seen = HashSet::new();
        assert_eq!(
            round_robin(&lists, 5, |id| seen.insert(id.to_string())),
            ["A1", "B1", "C1", "A2", "B2"]
        );
        let mut seen = HashSet::new();
        assert_eq!(
            round_robin(&lists, 100, |id| seen.insert(id.to_string())),
            ["A1", "B1", "C1", "A2", "B2", "A3", "A4"]
        );
        assert!(round_robin(&lists, 0, |_| true).is_empty());
        assert_eq!(
            round_robin(&lists, 3, |id| id.starts_with('A')),
            ["A1", "A1", "A2"]
        );
        assert!(round_robin(&[], 3, |_| true).is_empty());
    }

    #[test]
    fn assembles_a_report_from_real_documents() {
        // The real batch answer for requests 2.19.0, with details for five of its ten advisories (the others stay id-only).
        let ids = parse_osv_batch(BATCH_REQUESTS).remove(0);
        let details = details_of(&[
            GHSA_J8R2,
            GHSA_X84V,
            PYSEC_2018_28,
            PYSEC_2023_74,
            PYSEC_2026_2275,
        ]);
        let report = vulnerabilities_for("requests", &ids, &details);
        let got: Vec<(&str, Option<&str>)> = report
            .iter()
            .map(|v| (v.id.as_str(), v.severity.as_deref()))
            .collect();
        // PYSEC-2018-28 and PYSEC-2023-74 are twins of fetched GHSA entries; the GHSA-gc5v stub is covered by PYSEC-2026-2275's aliases.
        assert_eq!(
            got,
            [
                ("GHSA-x84v-xcm2-53pg", Some("HIGH")),
                ("GHSA-j8r2-6x86-q33q", Some("MODERATE")),
                ("PYSEC-2026-2275", Some("MODERATE")),
                ("GHSA-9hjg-9r4m-mvj7", None),
                ("GHSA-9wx4-h78v-vm56", None),
                ("PYSEC-2026-1872", None),
                ("PYSEC-2026-1873", None),
            ]
        );
        assert!(report[..3]
            .iter()
            .all(|v| v.affects("2.19.0") && !v.summary.is_empty() && !v.fixed_versions.is_empty()));
        assert!(report[3..].iter().all(|v| v.summary.is_empty()
            && v.affected_versions.is_empty()
            && v.reference.is_none()));
        assert_eq!(
            suggested_upgrade("2.19.0", &report).as_deref(),
            Some("2.33.0")
        );
        // Without any details every id is reported, sorted by id.
        let bare = vulnerabilities_for("requests", &ids, &HashMap::new());
        assert_eq!(bare.iter().map(|v| v.id.as_str()).collect::<Vec<_>>(), ids);
        assert!(vulnerabilities_for("requests", &[], &details).is_empty());
    }

    #[test]
    fn reports_drop_withdrawn_advisories_and_focus_on_the_package() {
        let details = details_of(&[
            r#"{"id": "OSV-W", "summary": "gone", "withdrawn": "2024-01-01T00:00:00Z"}"#,
            MULTI_JSON,
        ]);
        assert!(vulnerabilities_for("x", &strings(&["OSV-W"]), &details).is_empty());
        let report =
            vulnerabilities_for("demo-plugin", &strings(&["OSV-W", "OSV-TEST-1"]), &details);
        assert_eq!(report.len(), 1);
        assert_eq!(report[0].affected_versions, [">=0.5,<0.9"]);
        assert_eq!(report[0].fixed_versions, ["0.9"]);
    }

    #[test]
    fn collapses_twins_whatever_their_order() {
        let ghsa = parse_advisory(GHSA_X84V).unwrap();
        let pysec = parse_advisory(PYSEC_2018_28).unwrap();
        for list in [
            vec![ghsa.clone(), pysec.clone()],
            vec![pysec.clone(), ghsa.clone()],
        ] {
            let kept = collapse_aliases(list);
            assert_eq!(kept.len(), 1);
            assert_eq!(kept[0].id, "GHSA-x84v-xcm2-53pg");
        }
        // Only the PYSEC twin was fetched: it stands in for the GHSA stub.
        let kept = collapse_aliases(vec![Advisory::stub("GHSA-x84v-xcm2-53pg"), pysec]);
        assert_eq!(
            kept.iter().map(|a| a.id.as_str()).collect::<Vec<_>>(),
            ["PYSEC-2018-28"]
        );
        // Entries sharing just a CVE id: the one with a severity wins.
        let a = Advisory {
            id: "OSV-A".into(),
            summary: "a".into(),
            aliases: strings(&["CVE-1"]),
            ..Default::default()
        };
        let b = Advisory {
            id: "OSV-B".into(),
            summary: "b".into(),
            severity: Some("LOW".into()),
            aliases: strings(&["CVE-1"]),
            ..Default::default()
        };
        let kept = collapse_aliases(vec![a, b]);
        assert_eq!(
            kept.iter().map(|a| a.id.as_str()).collect::<Vec<_>>(),
            ["OSV-B"]
        );
        // Unrelated stubs are all kept.
        assert_eq!(
            collapse_aliases(vec![Advisory::stub("X-1"), Advisory::stub("X-2")]).len(),
            2
        );
    }

    #[test]
    fn suggests_upgrades() {
        let v = |ranges: &[&str], fixed: &[&str]| Vulnerability {
            id: "X".into(),
            affected_versions: strings(ranges),
            fixed_versions: strings(fixed),
            ..Default::default()
        };
        // Fixes on several release lines: the one of the installed line.
        let django = [v(
            &[">=3.2,<3.2.20", ">=4.0,<4.1.10", ">=4.2,<4.2.2"],
            &["3.2.20", "4.1.10", "4.2.2"],
        )];
        assert_eq!(
            suggested_upgrade("4.1.3", &django).as_deref(),
            Some("4.1.10")
        );
        assert_eq!(
            suggested_upgrade("3.2.1", &django).as_deref(),
            Some("3.2.20")
        );
        assert_eq!(suggested_upgrade("4.2.2", &django), None);
        // A final release is preferred to a pre-release, unless the installed version is a pre-release or nothing else qualifies.
        let pre = [v(&["<1.5rc1"], &["1.5rc1", "1.5"])];
        assert_eq!(suggested_upgrade("1.0", &pre).as_deref(), Some("1.5"));
        assert_eq!(suggested_upgrade("1.5b1", &pre).as_deref(), Some("1.5rc1"));
        assert_eq!(
            suggested_upgrade("1.0", &[v(&["<1.5rc1"], &["1.5rc1"])]).as_deref(),
            Some("1.5rc1")
        );
        // Unfixed advisories leave nothing to suggest; id-only stubs are ignored.
        assert_eq!(
            suggested_upgrade("1.0", &[v(&["<2.0"], &["2.0"]), v(&[">=0"], &[])]),
            None
        );
        assert_eq!(
            suggested_upgrade(
                "1.0",
                &[
                    v(&["<2.0"], &["2.0"]),
                    Vulnerability {
                        id: "STUB".into(),
                        ..Default::default()
                    }
                ]
            )
            .as_deref(),
            Some("2.0")
        );
        assert_eq!(suggested_upgrade("1.0", &[]), None);
    }
}

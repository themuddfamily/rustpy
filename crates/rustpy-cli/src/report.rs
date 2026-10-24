//! Result files of `rustpy inspect`, in the inspection report formats: one
//! `<Inspection>.xml` (or `.json`) per inspection with problems plus
//! `.descriptions.xml` (`.json`) listing the profile's inspections; `plain`
//! text; or one SARIF 2.1.0 log.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

use rustpy_core::Severity;
use serde_json::{json, Value};

use crate::args::ReportFormat;
use crate::inspect::{describe, Finding, CATALOG};
use crate::profile::Profile;

/// Severity names as written in XML profiles and reports.
pub fn severity_name(s: Severity) -> &'static str {
    match s {
        Severity::Error => "ERROR",
        Severity::Warning => "WARNING",
        Severity::WeakWarning => "WEAK WARNING",
        Severity::Info => "INFO",
        Severity::Typo => "TYPO",
    }
}

/// The editor's text attributes key for a problem, as XML reports write it.
fn attribute_key(id: &str, s: Severity) -> &'static str {
    match (id, s) {
        ("PyUnresolvedReferences", _) => "WRONG_REFERENCES_ATTRIBUTES",
        ("PyUnusedImports" | "PyUnusedLocal", _) => "NOT_USED_ELEMENT_ATTRIBUTES",
        (_, Severity::Error) => "ERRORS_ATTRIBUTES",
        (_, Severity::Warning) => "WARNING_ATTRIBUTES",
        (_, Severity::WeakWarning) => "INFO_ATTRIBUTES",
        (_, Severity::Typo) => "TYPO",
        (_, Severity::Info) => "INFORMATION_ATTRIBUTES",
    }
}

fn sarif_level(s: Severity) -> &'static str {
    match s {
        Severity::Error => "error",
        Severity::Warning => "warning",
        Severity::WeakWarning | Severity::Info | Severity::Typo => "note",
    }
}

fn language_of(rel: &str) -> &'static str {
    if rel.ends_with(".md") || rel.ends_with(".markdown") {
        "Markdown"
    } else {
        "Python"
    }
}

fn xml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '&' => out.push_str("&amp;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            // Characters XML 1.0 cannot hold.
            c if (c as u32) < 0x20 && !matches!(c, '\t' | '\n' | '\r') => out.push(' '),
            c => out.push(c),
        }
    }
    out
}

/// Percent-encodes a relative path for a URI (keeps `/`).
pub fn uri_path(rel: &str) -> String {
    let mut out = String::new();
    for b in rel.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// `file:///…/` URI of a folder.
pub fn folder_uri(dir: &Path) -> String {
    let mut p = dir.to_string_lossy().replace('\\', "/");
    if !p.starts_with('/') {
        p.insert(0, '/');
    }
    if !p.ends_with('/') {
        p.push('/');
    }
    format!("file://{}", uri_path(&p))
}

/// Findings grouped by inspection id (sorted).
fn by_inspection(findings: &[Finding]) -> BTreeMap<&str, Vec<&Finding>> {
    let mut map: BTreeMap<&str, Vec<&Finding>> = BTreeMap::new();
    for f in findings {
        map.entry(f.inspection.as_str()).or_default().push(f);
    }
    map
}

/// The inspections listed in the descriptions file: every catalog entry the
/// profile enables, plus any other id with findings.
fn described(
    profile: &Profile,
    findings: &[Finding],
) -> Vec<(String, String, &'static str, Severity, bool)> {
    let mut out: Vec<(String, String, &'static str, Severity, bool)> = CATALOG
        .iter()
        .map(|(id, name, group, sev)| {
            (
                id.to_string(),
                name.to_string(),
                *group,
                profile.severity_of(id, *sev),
                profile.is_enabled(id),
            )
        })
        .collect();
    for f in findings {
        if !out.iter().any(|(id, ..)| *id == f.inspection) {
            let (name, group) = describe(&f.inspection);
            out.push((f.inspection.clone(), name, group, f.severity, true));
        }
    }
    out
}

fn module_name(project: &Path) -> String {
    project
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "project".into())
}

fn problem_xml(f: &Finding, module: &str) -> String {
    let file = format!("file://$PROJECT_DIR$/{}", f.rel);
    let (name, _) = describe(&f.inspection);
    format!(
        "  <problem>\n    <file>{file}</file>\n    <line>{line}</line>\n    <module>{module}</module>\n    <entry_point TYPE=\"file\" FQNAME=\"{file}\" />\n    <problem_class id=\"{id}\" severity=\"{sev}\" attribute_key=\"{key}\">{name}</problem_class>\n    <description>{desc}</description>\n    <highlighted_element>{snippet}</highlighted_element>\n    <language>{lang}</language>\n    <offset>{offset}</offset>\n    <length>{length}</length>\n  </problem>\n",
        file = xml_escape(&file),
        line = f.line,
        module = xml_escape(module),
        id = xml_escape(&f.inspection),
        sev = severity_name(f.severity),
        key = attribute_key(&f.inspection, f.severity),
        name = xml_escape(&name),
        desc = xml_escape(&f.message),
        snippet = xml_escape(&f.snippet),
        lang = language_of(&f.rel),
        offset = f.column - 1,
        length = f.length,
    )
}

fn problem_json(f: &Finding, module: &str) -> Value {
    let file = format!("file://$PROJECT_DIR$/{}", f.rel);
    let (name, _) = describe(&f.inspection);
    json!({
        "tags": [],
        "entry_point": {"TYPE": "file", "FQNAME": file},
        "file": file,
        "line": f.line,
        "column": f.column,
        "offset": f.column - 1,
        "length": f.length,
        "module": module,
        "problem_class": {"id": f.inspection, "name": name, "severity": severity_name(f.severity), "attribute_key": attribute_key(&f.inspection, f.severity)},
        "description": f.message,
        "highlighted_element": f.snippet,
        "language": language_of(&f.rel),
        "path": f.rel,
    })
}

/// One line of the plain format: `path:line:column: SEVERITY: message [id]`.
pub fn plain_line(f: &Finding) -> String {
    format!(
        "{}:{}:{}: {}: {} [{}]",
        f.rel,
        f.line,
        f.column,
        severity_name(f.severity),
        f.message,
        f.inspection
    )
}

pub fn plain_text(findings: &[Finding]) -> String {
    findings.iter().map(|f| plain_line(f) + "\n").collect()
}

/// The SARIF 2.1.0 log of a run.
pub fn sarif(project: &Path, profile: &Profile, findings: &[Finding], files: usize) -> Value {
    let mut rules: Vec<(String, String, Severity)> = Vec::new();
    for f in findings {
        if !rules.iter().any(|(id, ..)| *id == f.inspection) {
            let default = CATALOG
                .iter()
                .find(|(id, ..)| *id == f.inspection)
                .map(|(.., s)| *s)
                .unwrap_or(f.severity);
            rules.push((
                f.inspection.clone(),
                describe(&f.inspection).0,
                profile.severity_of(&f.inspection, default),
            ));
        }
    }
    rules.sort();
    let rule_json: Vec<Value> = rules
        .iter()
        .map(|(id, name, sev)| {
            json!({
                "id": id,
                "name": id,
                "shortDescription": {"text": name},
                "defaultConfiguration": {"level": sarif_level(*sev)},
                "properties": {"tags": [describe(id).1], "severity": severity_name(*sev)},
            })
        })
        .collect();
    let results: Vec<Value> = findings
        .iter()
        .map(|f| {
            let index = rules
                .iter()
                .position(|(id, ..)| *id == f.inspection)
                .unwrap_or(0);
            json!({
                "ruleId": f.inspection,
                "ruleIndex": index,
                "level": sarif_level(f.severity),
                "message": {"text": f.message},
                "locations": [{
                    "physicalLocation": {
                        "artifactLocation": {"uri": uri_path(&f.rel), "uriBaseId": "PROJECT_DIR"},
                        "region": {
                            "startLine": f.line,
                            "startColumn": f.column,
                            "endLine": f.end_line,
                            "endColumn": f.end_column,
                            "snippet": {"text": f.snippet},
                        },
                    },
                }],
                "properties": {"severity": severity_name(f.severity)},
            })
        })
        .collect();
    json!({
        "$schema": "https://json.schemastore.org/sarif-2.1.0.json",
        "version": "2.1.0",
        "runs": [{
            "tool": {"driver": {
                "name": "RustPy",
                "fullName": "RustPy inspections",
                "version": crate::help::VERSION,
                "informationUri": "https://github.com/rustpy/rustpy",
                "rules": rule_json,
            }},
            "originalUriBaseIds": {"PROJECT_DIR": {"uri": folder_uri(project)}},
            "columnKind": "unicodeCodePoints",
            "automationDetails": {"id": format!("rustpy-inspect/{}/", profile.name)},
            "invocations": [{"executionSuccessful": true, "properties": {"profile": profile.name, "filesInspected": files}}],
            "results": results,
        }],
    })
}

fn write_file(path: &Path, text: &str) -> io::Result<()> {
    std::fs::write(path, text)
}

/// Whether `path` is a result file this module wrote (by name and content).
fn is_old_result(path: &Path, ext: &str) -> bool {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let Some(stem) = name.strip_suffix(&format!(".{ext}")) else {
        return false;
    };
    if stem != ".descriptions"
        && !(stem.chars().next().is_some_and(|c| c.is_ascii_uppercase())
            && stem.chars().all(|c| c.is_ascii_alphanumeric()))
    {
        return false;
    }
    let Ok(text) = std::fs::read_to_string(path) else {
        return false;
    };
    match ext {
        "xml" => {
            text.starts_with("<?xml")
                && (text.contains("<problems>") || text.contains("<inspections profile="))
        }
        _ => serde_json::from_str::<Value>(&text).is_ok_and(|v| {
            v.get("problems").is_some() || (v.get("profile").is_some() && v.get("groups").is_some())
        }),
    }
}

/// Removes result files of an earlier run, so the folder shows this run only.
fn clear_old_results(out_dir: &Path, ext: &str) -> io::Result<()> {
    let Ok(entries) = std::fs::read_dir(out_dir) else {
        return Ok(());
    };
    for e in entries.flatten() {
        let path = e.path();
        if e.file_type().is_ok_and(|t| t.is_file()) && is_old_result(&path, ext) {
            std::fs::remove_file(path)?;
        }
    }
    Ok(())
}

/// Writes the results of a run to `out_dir`; returns the files written.
pub fn write(
    format: ReportFormat,
    out_dir: &Path,
    project: &Path,
    profile: &Profile,
    findings: &[Finding],
    files: usize,
) -> io::Result<Vec<PathBuf>> {
    std::fs::create_dir_all(out_dir)?;
    let module = module_name(project);
    let mut written = Vec::new();
    match format {
        ReportFormat::Xml => {
            clear_old_results(out_dir, "xml")?;
            for (id, list) in by_inspection(findings) {
                let mut text =
                    String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<problems>\n");
                for f in list {
                    text.push_str(&problem_xml(f, &module));
                }
                text.push_str("</problems>\n");
                let path = out_dir.join(format!("{id}.xml"));
                write_file(&path, &text)?;
                written.push(path);
            }
            let mut groups: BTreeMap<&str, Vec<String>> = BTreeMap::new();
            for (id, name, group, sev, enabled) in described(profile, findings) {
                groups.entry(group).or_default().push(format!(
                    "    <inspection shortName=\"{}\" defaultSeverity=\"{}\" displayName=\"{}\" enabled=\"{enabled}\" />\n",
                    xml_escape(&id),
                    severity_name(sev),
                    xml_escape(&name)
                ));
            }
            let mut text = format!(
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<inspections profile=\"{}\">\n",
                xml_escape(&profile.name)
            );
            for (group, entries) in groups {
                text.push_str(&format!(
                    "  <group name=\"{0}\" path=\"{0}\">\n",
                    xml_escape(group)
                ));
                for e in entries {
                    text.push_str(&e);
                }
                text.push_str("  </group>\n");
            }
            text.push_str("</inspections>\n");
            let path = out_dir.join(".descriptions.xml");
            write_file(&path, &text)?;
            written.push(path);
        }
        ReportFormat::Json => {
            clear_old_results(out_dir, "json")?;
            for (id, list) in by_inspection(findings) {
                let doc = json!({"version": "3", "problems": list.iter().map(|f| problem_json(f, &module)).collect::<Vec<_>>()});
                let path = out_dir.join(format!("{id}.json"));
                write_file(
                    &path,
                    &serde_json::to_string_pretty(&doc).map_err(io::Error::other)?,
                )?;
                written.push(path);
            }
            let mut groups: BTreeMap<&str, Vec<Value>> = BTreeMap::new();
            for (id, name, group, sev, enabled) in described(profile, findings) {
                groups.entry(group).or_default().push(json!({"shortName": id, "displayName": name, "defaultSeverity": severity_name(sev), "enabled": enabled}));
            }
            let doc = json!({"profile": profile.name, "groups": groups.into_iter().map(|(name, inspections)| json!({"name": name, "inspections": inspections})).collect::<Vec<_>>()});
            let path = out_dir.join(".descriptions.json");
            write_file(
                &path,
                &serde_json::to_string_pretty(&doc).map_err(io::Error::other)?,
            )?;
            written.push(path);
        }
        ReportFormat::Plain => {
            let path = out_dir.join("inspections.txt");
            write_file(&path, &plain_text(findings))?;
            written.push(path);
        }
        ReportFormat::Sarif => {
            let path = out_dir.join("results.sarif");
            write_file(
                &path,
                &serde_json::to_string_pretty(&sarif(project, profile, findings, files))
                    .map_err(io::Error::other)?,
            )?;
            written.push(path);
        }
    }
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn finding(id: &str, sev: Severity, rel: &str, line: usize, message: &str) -> Finding {
        Finding {
            inspection: id.into(),
            severity: sev,
            path: PathBuf::from("/p").join(rel),
            rel: rel.into(),
            line,
            column: 3,
            end_line: line,
            end_column: 6,
            length: 3,
            message: message.into(),
            snippet: "foo".into(),
        }
    }

    #[test]
    fn escapes_and_uris() {
        assert_eq!(xml_escape("a<b>&\"'\u{1}"), "a&lt;b&gt;&amp;&quot;&apos; ");
        assert_eq!(
            uri_path("dir with space/ä.py"),
            "dir%20with%20space/%C3%A4.py"
        );
        assert_eq!(folder_uri(Path::new("/tmp/p q")), "file:///tmp/p%20q/");
    }

    #[test]
    fn xml_reports_replace_older_results() {
        let dir = tempfile::tempdir().unwrap();
        let profile = Profile::from_groups("Project Default", "test", Default::default());
        std::fs::write(
            dir.path().join("PyRedeclaration.xml"),
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<problems>\n</problems>\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("PyMine.xml"),
            "<?xml version=\"1.0\"?><config/>",
        )
        .unwrap();
        std::fs::write(dir.path().join("keep.txt"), "mine").unwrap();
        let findings = vec![
            finding(
                "PyUnusedLocal",
                Severity::Warning,
                "a.py",
                2,
                "Local variable 'x' value is not used",
            ),
            finding(
                "PyUnusedLocal",
                Severity::Warning,
                "b/c.py",
                7,
                "<odd> & \"quoted\"",
            ),
        ];
        let written = write(
            ReportFormat::Xml,
            dir.path(),
            Path::new("/p/proj"),
            &profile,
            &findings,
            2,
        )
        .unwrap();
        assert_eq!(
            written,
            vec![
                dir.path().join("PyUnusedLocal.xml"),
                dir.path().join(".descriptions.xml")
            ]
        );
        assert!(
            !dir.path().join("PyRedeclaration.xml").exists(),
            "results of an earlier run are removed"
        );
        assert!(
            dir.path().join("keep.txt").exists() && dir.path().join("PyMine.xml").exists(),
            "other files stay"
        );
        let xml = std::fs::read_to_string(dir.path().join("PyUnusedLocal.xml")).unwrap();
        assert_eq!(xml.matches("<problem>").count(), 2);
        assert!(
            xml.contains("<module>proj</module>")
                && xml.contains("<file>file://$PROJECT_DIR$/b/c.py</file>")
                && xml.contains("&lt;odd&gt; &amp; &quot;quoted&quot;"),
            "{xml}"
        );
        assert!(
            xml.contains("<offset>2</offset>") && xml.contains("<length>3</length>"),
            "{xml}"
        );
        assert!(crate::mini_xml::tags(&xml).is_ok(), "well-formed");
        let desc = std::fs::read_to_string(dir.path().join(".descriptions.xml")).unwrap();
        assert!(crate::mini_xml::tags(&desc).is_ok());
        assert!(desc.contains(r#"<inspection shortName="PyMissingOrEmptyDocstring" defaultSeverity="WEAK WARNING" displayName="Missing or empty docstring" enabled="false" />"#), "{desc}");
    }

    #[test]
    fn sarif_log_structure() {
        let profile = Profile::from_groups("CI", "test", Default::default());
        let findings = vec![
            finding(
                "PyUnresolvedReferences",
                Severity::Error,
                "src/m.py",
                4,
                "Unresolved reference 'x'",
            ),
            finding(
                "PyPep8",
                Severity::WeakWarning,
                "src/m.py",
                9,
                "PEP 8: E501 line too long",
            ),
        ];
        let log = sarif(Path::new("/w/proj"), &profile, &findings, 1);
        let run = &log["runs"][0];
        assert_eq!(
            run["originalUriBaseIds"]["PROJECT_DIR"]["uri"],
            "file:///w/proj/"
        );
        let rules = run["tool"]["driver"]["rules"].as_array().unwrap();
        assert_eq!(rules.len(), 2);
        for r in run["results"].as_array().unwrap() {
            let idx = r["ruleIndex"].as_u64().unwrap() as usize;
            assert_eq!(rules[idx]["id"], r["ruleId"]);
        }
        assert_eq!(run["results"][0]["level"], "error");
        assert_eq!(run["results"][1]["level"], "note");
        assert_eq!(
            run["results"][0]["locations"][0]["physicalLocation"]["region"]["endColumn"],
            6
        );
    }

    #[test]
    fn plain_lines() {
        let f = finding(
            "PyUnusedImports",
            Severity::Warning,
            "a/x.py",
            1,
            "Unused import statement 'import os'",
        );
        assert_eq!(
            plain_line(&f),
            "a/x.py:1:3: WARNING: Unused import statement 'import os' [PyUnusedImports]"
        );
    }
}

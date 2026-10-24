//! `requirements.txt` parsing and "is this requirement satisfied" checks.

use serde::{Deserialize, Serialize};

use crate::version::{parse_version, specifier_matches};
use crate::InstalledPackage;

/// One requirement line of a `requirements.txt`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Requirement {
    pub name: String,
    /// Version specifier set without spaces, e.g. `>=1.2,<2`.
    pub specifier: Option<String>,
    pub extras: Vec<String>,
    /// Environment marker after `;`, e.g. `python_version < "3.8"`.
    pub marker: Option<String>,
    /// 1-based line number of the (first) physical line.
    pub line: usize,
    /// The logical line as written (continuations joined, comments stripped).
    pub raw: String,
}

/// PEP 503 name normalization: lowercase, runs of `-`, `_` and `.` become `-`.
pub fn normalize_name(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut in_sep = false;
    for c in name.trim().chars() {
        if matches!(c, '-' | '_' | '.') {
            in_sep = true;
        } else {
            if in_sep {
                out.push('-');
                in_sep = false;
            }
            out.extend(c.to_lowercase());
        }
    }
    out
}

/// Extract only syntactically valid named dependencies; preserve the existing consumer API.
pub fn parse_requirements(text: &str) -> Vec<Requirement> {
    crate::requirements_file::parse(text)
        .records
        .into_iter()
        .filter_map(|record| {
            let crate::requirements_file::Syntax::Requirement {
                target: crate::requirements_file::Target::Named(_),
                requirement,
                editable: false,
                ..
            } = record.syntax
            else {
                return None;
            };
            parse_requirement(&requirement, record.line)
        })
        .collect()
}

/// Parse one named PEP 508 dependency without performing URL or include I/O.
pub fn parse_requirement(raw: &str, line: usize) -> Option<Requirement> {
    use pep508_rs::VersionOrUrl;
    let raw = raw.trim();
    let parsed = crate::requirements_file::parse_named(raw).ok()?;
    // Keep original spelling for API compatibility; the structured name is normalized.
    let name_end = raw
        .find(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-')))
        .unwrap_or(raw.len());
    let specifier = match &parsed.version_or_url {
        Some(VersionOrUrl::VersionSpecifier(_)) => {
            let mut tail = raw[name_end..].trim_start();
            if tail.starts_with('[') {
                tail = tail[tail.find(']')? + 1..].trim_start();
            }
            Some(
                tail.split(';')
                    .next()?
                    .chars()
                    .filter(|c| !c.is_whitespace() && !matches!(c, '(' | ')'))
                    .collect(),
            )
        }
        _ => None,
    };
    // MarkerTree is normalized semantically; the legacy API retains the original marker text.
    let marker_start = if matches!(parsed.version_or_url, Some(VersionOrUrl::Url(_))) {
        raw.char_indices()
            .find(|&(i, c)| c == ';' && i > 0 && raw.as_bytes()[i - 1].is_ascii_whitespace())
            .map(|(i, _)| i)
    } else {
        raw.find(';')
    };
    Some(Requirement {
        name: raw[..name_end].into(),
        specifier,
        extras: parsed.extras.iter().map(ToString::to_string).collect(),
        marker: marker_start.map(|i| raw[i + 1..].trim().to_string()),
        line,
        raw: raw.into(),
    })
}

/// True when an installed version satisfies the requirement's specifier.
pub fn is_satisfied_by(req: &Requirement, installed_version: &str) -> bool {
    match &req.specifier {
        None => true,
        Some(spec) => specifier_matches(&parse_version(installed_version), spec),
    }
}

/// Requirements that are missing (`None`) or installed in a version that does
/// not match the specifier (`Some(installed version)`).
pub fn unsatisfied(
    reqs: &[Requirement],
    installed: &[InstalledPackage],
) -> Vec<(Requirement, Option<String>)> {
    let mut out = Vec::new();
    for req in reqs {
        let key = normalize_name(&req.name);
        match installed.iter().find(|p| normalize_name(&p.name) == key) {
            None => out.push((req.clone(), None)),
            Some(p) => {
                if !is_satisfied_by(req, &p.version) {
                    out.push((req.clone(), Some(p.version.clone())));
                }
            }
        }
    }
    out
}

/// Evaluate markers against the selected interpreter's captured PEP508 world.
/// Missing fields or malformed data are unknown, never evidence for installation.
pub fn applies_in_environment(
    req: &Requirement,
    values: &[(String, String)],
    active_extras: &[String],
) -> Option<bool> {
    if req.marker.is_none() {
        return Some(true);
    }
    let get = |name: &str| {
        values
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    };
    let environment: pep508_rs::MarkerEnvironment = pep508_rs::MarkerEnvironmentBuilder {
        implementation_name: get("implementation_name")?,
        implementation_version: get("implementation_version")?,
        os_name: get("os_name")?,
        platform_machine: get("platform_machine")?,
        platform_python_implementation: get("platform_python_implementation")?,
        platform_release: get("platform_release")?,
        platform_system: get("platform_system")?,
        platform_version: get("platform_version")?,
        python_full_version: get("python_full_version")?,
        python_version: get("python_version")?,
        sys_platform: get("sys_platform")?,
    }
    .try_into()
    .ok()?;
    let extras = active_extras
        .iter()
        .map(|extra| extra.parse::<pep508_rs::ExtraName>().ok())
        .collect::<Option<Vec<_>>>()?;
    let parsed = crate::requirements_file::parse_named(&req.raw).ok()?;
    Some(parsed.evaluate_markers(&environment, &extras))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn normalizes_names() {
        assert_eq!(normalize_name("Django"), "django");
        assert_eq!(normalize_name("zope.interface"), "zope-interface");
        assert_eq!(normalize_name("Zope_Interface"), "zope-interface");
        assert_eq!(normalize_name("foo__bar--baz..qux"), "foo-bar-baz-qux");
        assert_eq!(normalize_name(" typing-extensions "), "typing-extensions");
    }

    #[test]
    fn parses_requirement_lines() {
        let text = "# deps\nrequests>=2.0,<3 # http\nDjango == 4.2 ; python_version >= \"3.8\"\n-r other.txt\n-e .\n--index-url https://example.invalid/simple\nnumpy[all,extra] ~= 1.26\nflask\n\n  \nblack @ https://example.invalid/black.whl\ngit+https://github.com/x/y.git#egg=y\n./local/path\nurllib3 (>=1.21.1)\ncoverage>=7 \\\n  ,<8\n";
        let reqs = parse_requirements(text);
        let names: Vec<&str> = reqs.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["requests", "Django", "numpy", "flask", "black", "urllib3", "coverage"]
        );
        assert_eq!(reqs[0].specifier.as_deref(), Some(">=2.0,<3"));
        assert_eq!(reqs[0].line, 2);
        assert_eq!(reqs[0].raw, "requests>=2.0,<3");
        assert_eq!(reqs[1].specifier.as_deref(), Some("==4.2"));
        assert_eq!(reqs[1].marker.as_deref(), Some("python_version >= \"3.8\""));
        assert_eq!(reqs[2].extras, vec!["all", "extra"]);
        assert_eq!(reqs[2].specifier.as_deref(), Some("~=1.26"));
        assert_eq!(reqs[3].specifier, None);
        assert_eq!(reqs[4].specifier, None);
        assert_eq!(reqs[5].specifier.as_deref(), Some(">=1.21.1"));
        assert_eq!(reqs[6].specifier.as_deref(), Some(">=7,<8"));
        assert_eq!(reqs[6].line, 15);
    }

    #[test]
    fn reports_unsatisfied() {
        let reqs =
            parse_requirements("requests>=2.0\nDjango==4.2\nzope.interface\nmissing-pkg>=1\n");
        let installed = vec![
            InstalledPackage {
                name: "Requests".into(),
                version: "2.31.0".into(),
                location: Some(PathBuf::from("/x")),
                editable: false,
                summary: None,
            },
            InstalledPackage {
                name: "django".into(),
                version: "5.0".into(),
                location: None,
                editable: false,
                summary: None,
            },
            InstalledPackage {
                name: "zope_interface".into(),
                version: "6.0".into(),
                location: None,
                editable: false,
                summary: None,
            },
        ];
        let bad = unsatisfied(&reqs, &installed);
        assert_eq!(bad.len(), 2);
        assert_eq!(bad[0].0.name, "Django");
        assert_eq!(bad[0].1.as_deref(), Some("5.0"));
        assert_eq!(bad[1].0.name, "missing-pkg");
        assert_eq!(bad[1].1, None);
    }
}

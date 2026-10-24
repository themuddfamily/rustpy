//! CHANGELOG.md sections ("Keep a Changelog" layout): What's New shows the section of the
//! running version, and the release workflow attaches the same text to the GitHub release
//! (`scripts/release-notes.sh` implements the same lookup for CI).

use crate::version::Version;

/// The label of a `## …` heading: `0.2.0` for `## [0.2.0] - 2026-10-01`, `## v0.2.0` or
/// `## 0.2.0`; `Unreleased` for `## [Unreleased]`.
fn heading_label(line: &str) -> Option<&str> {
    let rest = line.strip_prefix("## ")?.trim_start();
    let rest = rest.strip_prefix('[').unwrap_or(rest);
    let end = rest
        .find(|c: char| c == ']' || c.is_whitespace())
        .unwrap_or(rest.len());
    let label = &rest[..end];
    (!label.is_empty()).then_some(label)
}

fn label_matches(label: &str, wanted: &str) -> bool {
    if wanted.eq_ignore_ascii_case("unreleased") {
        return label.eq_ignore_ascii_case("unreleased");
    }
    match (Version::parse(label), Version::parse(wanted)) {
        (Some(a), Some(b)) => a == b,
        _ => false,
    }
}

/// Whether `line` opens or closes a fenced code block (where `## ` is not a heading).
fn is_fence(line: &str) -> bool {
    let t = line.trim_start();
    t.starts_with("```") || t.starts_with("~~~")
}

/// A link reference definition such as `[0.2.0]: https://github.com/…/compare/v0.1.0...v0.2.0`.
fn is_link_definition(line: &str) -> bool {
    let t = line.trim();
    t.starts_with('[')
        && t.split_once("]:").is_some_and(|(label, url)| {
            !label.contains(']') && !url.trim().is_empty() && !url.trim().contains(' ')
        })
}

/// The Markdown under the `## ` heading for `wanted` (a version, or `Unreleased`), up to
/// the next `## ` (or `# `) heading, without the trailing link definitions. `None` when
/// the changelog has no such section or it is empty.
pub fn section(changelog: &str, wanted: &str) -> Option<String> {
    let mut in_fence = false;
    let mut body: Option<Vec<&str>> = None;
    for line in changelog.lines() {
        let heading = !in_fence && (line.starts_with("## ") || line.starts_with("# "));
        if is_fence(line) {
            in_fence = !in_fence;
        }
        if heading {
            if body.is_some() {
                break;
            }
            if heading_label(line).is_some_and(|l| label_matches(l, wanted)) {
                body = Some(Vec::new());
            }
            continue;
        }
        if let Some(b) = &mut body {
            b.push(line);
        }
    }
    let mut lines = body?;
    while lines
        .last()
        .is_some_and(|l| l.trim().is_empty() || is_link_definition(l))
    {
        lines.pop();
    }
    let start = lines.iter().position(|l| !l.trim().is_empty())?;
    Some(lines[start..].join("\n"))
}

/// Release notes for `version` with fallbacks: its own section, then the section of the
/// release it leads to (`0.3.0` for `0.3.0-eap.1`), then `Unreleased` (a development
/// build). Returns the label of the section used and its Markdown.
pub fn notes_for(changelog: &str, version: &str) -> Option<(String, String)> {
    let mut candidates = vec![version.trim().to_string()];
    if let Some(v) = Version::parse(version).filter(|v| v.is_prerelease()) {
        candidates.push(v.release().to_string());
    }
    candidates.push("Unreleased".into());
    candidates
        .into_iter()
        .find_map(|c| section(changelog, &c).map(|s| (c, s)))
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOG: &str = "# Changelog\n\nAll notable changes.\n\n## [Unreleased]\n\n### Added\n- Updates page\n\n## [0.2.0] - 2026-10-01\n\n### Added\n- Check for Updates\n\n```text\n## not a heading\n```\n\n## v0.1.1\n- Fixes\n\n## 0.1.0\n\n- First release\n\n[Unreleased]: https://github.com/o/r/compare/v0.2.0...HEAD\n[0.2.0]: https://github.com/o/r/compare/v0.1.1...v0.2.0\n";

    #[test]
    fn finds_sections_by_version_and_unreleased() {
        assert_eq!(
            section(LOG, "Unreleased").as_deref(),
            Some("### Added\n- Updates page")
        );
        assert_eq!(
            section(LOG, "unreleased").as_deref(),
            Some("### Added\n- Updates page")
        );
        assert_eq!(
            section(LOG, "0.2.0").as_deref(),
            Some("### Added\n- Check for Updates\n\n```text\n## not a heading\n```")
        );
        assert_eq!(
            section(LOG, "v0.2.0").as_deref(),
            Some("### Added\n- Check for Updates\n\n```text\n## not a heading\n```")
        );
        assert_eq!(section(LOG, "0.1.1").as_deref(), Some("- Fixes"));
        // The last section ends before the link definitions.
        assert_eq!(section(LOG, "0.1.0").as_deref(), Some("- First release"));
        assert_eq!(section(LOG, "0.3.0"), None);
        assert_eq!(section(LOG, "Changelog"), None);
        assert_eq!(section("## [1.0.0]\n\n## [0.9.0]\n- x\n", "1.0.0"), None);
    }

    #[test]
    fn falls_back_to_the_release_and_to_unreleased() {
        assert_eq!(
            notes_for(LOG, "0.2.0").map(|(l, _)| l),
            Some("0.2.0".into())
        );
        assert_eq!(
            notes_for(LOG, "0.2.0-eap.1").map(|(l, _)| l),
            Some("0.2.0".into())
        );
        assert_eq!(
            notes_for(LOG, "0.4.0"),
            Some(("Unreleased".into(), "### Added\n- Updates page".into()))
        );
        assert_eq!(notes_for("# Changelog\n", "0.1.0"), None);
    }

    /// Every `## ` heading of CHANGELOG.md must name `Unreleased` or a version, or What's New
    /// and the GitHub release notes cannot find its section.
    #[test]
    fn repository_changelog_headings_are_versions() {
        let log = include_str!("../../../CHANGELOG.md");
        assert!(
            log.starts_with("# Changelog"),
            "CHANGELOG.md starts with '# Changelog'"
        );
        let mut in_fence = false;
        for line in log.lines() {
            if is_fence(line) {
                in_fence = !in_fence;
            }
            if in_fence || !line.starts_with("## ") {
                continue;
            }
            let label = heading_label(line).unwrap_or_default();
            assert!(
                label.eq_ignore_ascii_case("unreleased") || Version::parse(label).is_some(),
                "CHANGELOG.md heading {line:?} is neither [Unreleased] nor a version"
            );
        }
    }
}

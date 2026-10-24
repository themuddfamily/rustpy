//! Live check against the OSV API (skipped when OSV cannot be reached).

use rustpy_packages::{
    check_requirements, check_vulnerabilities, parse_requirements, parse_version, severity_rank,
    suggested_upgrade,
};

/// True for errors that mean OSV is unusable right now (offline, timeouts,
/// server trouble) rather than a bug in the client.
fn unavailable(err: &anyhow::Error) -> bool {
    let msg = format!("{err:#}");
    msg.starts_with("cannot reach OSV")
        || msg.starts_with("OSV returned HTTP 5")
        || msg.starts_with("OSV returned HTTP 429")
}

#[test]
fn finds_known_vulnerabilities_of_requests_2_19_0() {
    let packages = vec![
        ("requests".to_string(), "2.19.0".to_string()),
        (
            "definitely-not-a-package-xyz-123".to_string(),
            "1.0".to_string(),
        ),
    ];
    let results = match check_vulnerabilities(&packages) {
        Ok(results) => results,
        Err(e) if unavailable(&e) => {
            eprintln!("OSV unreachable ({e:#}); skipping");
            return;
        }
        Err(e) => panic!("OSV check failed: {e:#}"),
    };
    assert_eq!(results.len(), 1, "{results:#?}");
    let (name, vulns) = &results[0];
    assert_eq!(name, "requests");
    assert!(!vulns.is_empty());

    let mut ids: Vec<&str> = vulns.iter().map(|v| v.id.as_str()).collect();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), vulns.len(), "duplicate ids: {vulns:#?}");
    let ranks: Vec<u8> = vulns
        .iter()
        .map(|v| severity_rank(v.severity.as_deref().unwrap_or("")))
        .collect();
    assert!(
        ranks.windows(2).all(|w| w[0] >= w[1]),
        "not sorted by severity: {vulns:#?}"
    );

    // Requests 2.19.0 has far fewer advisories than the lookup cap, so all details are fetched.
    let detailed: Vec<_> = vulns.iter().filter(|v| !v.summary.is_empty()).collect();
    assert!(!detailed.is_empty(), "no advisory details: {vulns:#?}");
    for v in &detailed {
        assert!(v.affects("2.19.0"), "{v:#?}");
    }
    assert!(
        detailed
            .iter()
            .any(|v| severity_rank(v.severity.as_deref().unwrap_or("")) > 0
                && !v.fixed_versions.is_empty()
                && v.reference.is_some()),
        "{vulns:#?}"
    );

    // CVE-2018-18074 (credentials leaked on https-to-http redirects, fixed in 2.20.0) is
    // listed by GitHub and PyPA; the twins are reported once.
    let cve_2018_18074 = vulns
        .iter()
        .filter(|v| v.id == "GHSA-x84v-xcm2-53pg" || v.id == "PYSEC-2018-28")
        .count();
    if detailed.len() == vulns.len() {
        assert_eq!(cve_2018_18074, 1, "{vulns:#?}");
    } else {
        assert!(cve_2018_18074 >= 1, "{vulns:#?}");
    }

    if let Some(target) = suggested_upgrade("2.19.0", vulns) {
        assert!(parse_version(&target) > parse_version("2.19.0"), "{target}");
        assert!(
            detailed.iter().all(|v| !v.affects(&target)),
            "{target} is still affected"
        );
    }

    // Pinned requirements are checked at their pin; unpinned, uninstalled ones are skipped.
    let reqs = parse_requirements("requests==2.19.0\nflask>=2\n");
    let checked = match check_requirements(&reqs, &[]) {
        Ok(checked) => checked,
        Err(e) if unavailable(&e) => {
            eprintln!("OSV unreachable ({e:#}); skipping the requirements check");
            return;
        }
        Err(e) => panic!("OSV requirements check failed: {e:#}"),
    };
    assert_eq!(checked.len(), 1, "{checked:#?}");
    let (req, version, req_vulns) = &checked[0];
    assert_eq!(
        (req.name.as_str(), req.line, version.as_str()),
        ("requests", 1, "2.19.0")
    );
    assert!(!req_vulns.is_empty());
}

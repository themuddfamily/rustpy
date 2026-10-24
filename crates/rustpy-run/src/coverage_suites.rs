//! Coverage suite union and portable exports, independent of interpreter IO.
use crate::coverage::{percent_of, CoverageReport, FileCoverage};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

pub fn merge(reports: impl IntoIterator<Item = CoverageReport>) -> CoverageReport {
    let mut files = BTreeMap::<PathBuf, FileCoverage>::new();
    let mut branch = false;
    for report in reports {
        branch |= report.branch;
        for incoming in report.files {
            let file = files
                .entry(incoming.path.clone())
                .or_insert_with(|| FileCoverage {
                    path: incoming.path.clone(),
                    ..Default::default()
                });
            for (target, source) in [
                (&mut file.executed, incoming.executed),
                (&mut file.missing, incoming.missing),
                (&mut file.excluded, incoming.excluded),
                (&mut file.partial, incoming.partial),
            ] {
                target.extend(source);
                target.sort_unstable();
                target.dedup();
            }
            file.executed_arcs.extend(incoming.executed_arcs);
            file.missing_arcs.extend(incoming.missing_arcs);
            // XML imports expose aggregate branches only. Avoid claiming unknown
            // complementary branches were covered by a different XML suite.
            file.num_branches = file.num_branches.max(incoming.num_branches);
            file.covered_branches = file.covered_branches.max(incoming.covered_branches);
        }
    }
    for file in files.values_mut() {
        file.missing
            .retain(|line| file.executed.binary_search(line).is_err());
        file.excluded.retain(|line| {
            file.executed.binary_search(line).is_err() && file.missing.binary_search(line).is_err()
        });
        let covered: BTreeSet<_> = file.executed_arcs.iter().copied().collect();
        let missing: BTreeSet<_> = file
            .missing_arcs
            .iter()
            .copied()
            .filter(|arc| !covered.contains(arc))
            .collect();
        if !covered.is_empty() || !missing.is_empty() {
            file.covered_branches = covered.len();
            file.num_branches = covered.len() + missing.len();
            file.partial = missing
                .iter()
                .filter_map(|(from, _)| usize::try_from(*from).ok())
                .filter(|line| file.executed.binary_search(line).is_ok())
                .collect();
            file.partial.sort_unstable();
            file.partial.dedup();
        }
        file.executed_arcs = covered.into_iter().collect();
        file.missing_arcs = missing.into_iter().collect();
        if file.num_branches == file.covered_branches {
            file.partial.clear();
        }
        file.percent = percent_of(
            file.executed.len() + file.covered_branches,
            file.statements() + file.num_branches,
        );
    }
    let mut report = CoverageReport {
        files: files.into_values().collect(),
        branch,
        ..Default::default()
    };
    let (lc, lt) = report.totals();
    let (bc, bt) = report.branch_totals();
    report.total_percent = percent_of(lc + bc, lt + bt);
    report
}
pub fn export_json(report: &CoverageReport, path: &Path) -> anyhow::Result<()> {
    std::fs::write(
        path,
        serde_json::to_vec_pretty(&serde_json::json!({"rustpy_coverage":1,"report":report}))?,
    )?;
    Ok(())
}
fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}
/// Portable HTML works for merged and XML-only suites as well as SQLite data.
pub fn export_html(report: &CoverageReport, out: &Path) -> anyhow::Result<PathBuf> {
    std::fs::create_dir_all(out)?;
    let mut index=String::from("<!doctype html><meta charset=utf-8><title>Python coverage</title><h1>Python coverage</h1><table><tr><th>File</th><th>Statements</th><th>Coverage</th></tr>");
    for (number, file) in report.files.iter().enumerate() {
        let page = format!("file-{number}.html");
        index.push_str(&format!(
            "<tr><td><a href='{page}'>{}</a></td><td>{}/{}</td><td>{:.1}%</td></tr>",
            escape(&file.path.display().to_string()),
            file.executed.len(),
            file.statements(),
            file.percent
        ));
        let source = std::fs::read_to_string(&file.path).unwrap_or_default();
        let mut html=format!("<!doctype html><meta charset=utf-8><title>{}</title><style>pre{{margin:0}}.covered{{background:#d8f5d8}}.missed{{background:#ffdada}}.partial{{background:#fff1ba}}</style><h1>{}</h1>",escape(&file.path.display().to_string()),escape(&file.path.display().to_string()));
        for (line, text) in source.lines().enumerate() {
            let class = match file.line_state(line + 1) {
                Some(crate::coverage::LineCoverage::Covered) => "covered",
                Some(crate::coverage::LineCoverage::Missed) => "missed",
                Some(crate::coverage::LineCoverage::Partial) => "partial",
                _ => "",
            };
            html.push_str(&format!(
                "<pre class='{class}'>{:5} {}</pre>",
                line + 1,
                escape(text)
            ));
        }
        std::fs::write(out.join(page), html)?;
    }
    index.push_str("</table>");
    let path = out.join("index.html");
    std::fs::write(&path, index)?;
    Ok(path)
}

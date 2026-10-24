//! Saved structural templates participate in the ordinary suppression/fix pipeline.
use super::{Analysis, FileInfo, Problem, QuickFix};
use rustpy_core::{CancellationToken, Cancelled, Diagnostic, Severity};
use rustpy_search::structural::{self, Template};
use std::sync::Arc;

pub(super) fn run(
    db: &Analysis,
    file: &Arc<FileInfo>,
    rules: &[Template],
    token: &CancellationToken,
    out: &mut Vec<Problem>,
) -> Result<(), Cancelled> {
    let suppressions = super::Suppressions::new(file);
    for rule in rules.iter().filter(|r| r.inspection).take(128) {
        token.check()?;
        let Ok(pattern) = structural::compile(&rule.pattern, &rule.options) else {
            continue;
        };
        for matched in structural::search_with_analysis(&pattern, db, file)
            .into_iter()
            .take(2000)
        {
            token.check()?;
            let fixes = if !rule.replace {
                Vec::new()
            } else {
                vec![QuickFix {
                    title: format!("Replace: {}", rule.name),
                    edits: vec![(
                        matched.range,
                        structural::expand_replacement(&rule.replacement, &matched, &file.text),
                    )],
                }]
            };
            let problem = Problem {
                diagnostic: Diagnostic::new(
                    matched.range,
                    Severity::WeakWarning,
                    "StructuralSearchInspection",
                    format!("Structural search: {}", rule.name),
                ),
                fixes,
            };
            if !suppressions.is_suppressed(&problem.diagnostic) {
                out.push(problem);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inspections::{inspect, InspectionOptions};
    use rustpy_search::structural::{Constraint, PatternOptions};
    use std::path::Path;

    #[test]
    fn configured_inspection_uses_filters_replacement_and_normal_suppression() {
        let db = Analysis::for_project(Path::new("/"));
        let path = Path::new("/structural_rule.py");
        db.set_file_text(
            path,
            "print('π')\nprint(42)\n# noinspection StructuralSearchInspection\nprint('hidden')\n",
        );
        let file = db.file_for_path(path).unwrap();
        let mut options = PatternOptions::default();
        options.constraints.insert(
            "value".into(),
            Constraint {
                type_regex: Some("str".into()),
                ..Default::default()
            },
        );
        let opts = InspectionOptions {
            structural_rules: Arc::new(vec![Template {
                name: "Use logging".into(),
                pattern: "print($value$)".into(),
                replacement: "logger.info($value$)".into(),
                replace: true,
                options,
                inspection: true,
            }]),
            ..Default::default()
        };
        let problems = inspect(&db, &file, &opts);
        let structural: Vec<_> = problems
            .iter()
            .filter(|p| p.diagnostic.code == "StructuralSearchInspection")
            .collect();
        assert_eq!(structural.len(), 1);
        assert_eq!(structural[0].fixes[0].edits[0].1, "logger.info('π')");
        assert!(inspect(&db, &file, &InspectionOptions::default())
            .iter()
            .all(|p| p.diagnostic.code != "StructuralSearchInspection"));
    }
}

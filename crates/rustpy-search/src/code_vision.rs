//! Batched resolver-backed Code Vision, using an owned project-index snapshot.
//! No disk walk and no textual-name counts: names only prune semantic queries.
use rustpy_core::{Cancelled, ProgressReporter, TextRange};
use rustpy_semantic::{infer::Inferer, types::Type, usages::UsageQuery, Analysis, FileInfo};
use rustpy_syntax::ast::{AstNode, ClassDef, FuncDef};
use std::{
    collections::{BTreeSet, HashMap, HashSet},
    path::PathBuf,
    sync::Arc,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UsageLens {
    pub range: TextRange,
    pub declaration: TextRange,
    pub name: String,
    pub line: usize,
    pub usages: usize,
}
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub declarations: usize,
    pub files: usize,
    pub source_bytes: usize,
    pub references: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            declarations: 128,
            files: 10_000,
            source_bytes: 64 * 1024 * 1024,
            references: 100_000,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unavailable {
    Cancelled,
    BudgetExceeded,
    SourceUnavailable,
}
impl From<Cancelled> for Unavailable {
    fn from(_: Cancelled) -> Self {
        Self::Cancelled
    }
}

/// A missing/incomplete scan returns an error, never misleading zero/partial
/// counts. Call only in a trusted background job, with the actual scoped index's
/// complete Python candidate list and the active editor's analysis snapshot.
pub fn usage_lenses(
    db: &Analysis,
    file: &Arc<FileInfo>,
    project_files: &[PathBuf],
    limits: Limits,
    progress: &ProgressReporter,
) -> Result<Vec<UsageLens>, Unavailable> {
    progress.check()?;
    if file.is_stub || file.text.len() > 1024 * 1024 {
        return Err(Unavailable::BudgetExceeded);
    }
    let root = file.parse.syntax_node();
    let mut declarations = Vec::new();
    for node in root.descendants() {
        progress.check()?;
        let declaration = node.text_range();
        let name = FuncDef::cast(node.clone())
            .and_then(|function| function.name())
            .or_else(|| ClassDef::cast(node).and_then(|class| class.name()));
        let Some(name) = name else {
            continue;
        };
        if declarations.len() == limits.declarations {
            return Err(Unavailable::BudgetExceeded);
        }
        let Some(query) = UsageQuery::prepare(db, file, name.text_range().start()) else {
            continue;
        };
        declarations.push((
            UsageLens {
                range: name.text_range(),
                declaration,
                name: name.text().into(),
                line: file.text[..usize::from(name.text_range().start())]
                    .bytes()
                    .filter(|byte| *byte == b'\n')
                    .count(),
                usages: 0,
            },
            query,
        ));
    }
    if declarations.is_empty() {
        return Ok(Vec::new());
    }
    let mut candidates: BTreeSet<PathBuf> = project_files
        .iter()
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "py" || extension == "pyi")
        })
        .cloned()
        .collect();
    candidates.insert(file.path.clone());
    for (_, query) in &declarations {
        candidates.extend(query.seed_files().iter().map(|seed| seed.path.clone()));
    }
    if candidates.len() > limits.files {
        return Err(Unavailable::BudgetExceeded);
    }
    let targets: HashMap<_, _> = declarations
        .iter()
        .enumerate()
        .map(|(index, (lens, _))| (lens.declaration, index))
        .collect();
    let inferer = Inferer::new(db);
    let mut matches: Vec<HashSet<(PathBuf, TextRange)>> =
        (0..declarations.len()).map(|_| HashSet::new()).collect();
    let mut bytes = file.text.len();
    if bytes > limits.source_bytes {
        return Err(Unavailable::BudgetExceeded);
    }
    let mut references = 0usize;
    for (scanned, path) in candidates.iter().enumerate() {
        progress.check()?;
        progress.set_step(scanned + 1, candidates.len());
        let candidate = if *path == file.path {
            file.clone()
        } else {
            let Some(text) = db.current_text(path) else {
                return Err(Unavailable::SourceUnavailable);
            };
            bytes = bytes.saturating_add(text.len());
            if bytes > limits.source_bytes {
                return Err(Unavailable::BudgetExceeded);
            }
            if !text.contains("import")
                && !declarations
                    .iter()
                    .any(|(_, query)| query.candidate_text_may_match(&text))
            {
                continue;
            }
            if text.len() > 1024 * 1024 {
                return Err(Unavailable::BudgetExceeded);
            }
            db.file_for_path(path)
                .ok_or(Unavailable::SourceUnavailable)?
        };
        // The rename-oriented usage query intentionally stops at aliases that
        // introduce a different local name. Code Vision also counts those real
        // reads, using the resolved callable/class identity rather than spelling.
        for reference in &candidate.scopes.references {
            progress.check()?;
            let Some(definition) =
                inferer.resolve_name(&candidate, reference.scope, &reference.name)
            else {
                continue;
            };
            let identity = match inferer.type_of_binding(&definition.file, definition.binding) {
                Type::Function(signature) => signature
                    .def
                    .as_ref()
                    .map(|function| (function.file, function.range)),
                Type::Class(class, _) => Some((class.file, class.range)),
                _ => None,
            };
            let Some((_, range)) = identity.filter(|(owner, _)| *owner == file.id) else {
                continue;
            };
            let Some(&index) = targets.get(&range) else {
                continue;
            };
            if matches[index].insert((candidate.path.clone(), reference.range)) {
                references += 1;
                if references > limits.references {
                    return Err(Unavailable::BudgetExceeded);
                }
            }
        }
        for (index, (_, query)) in declarations.iter().enumerate() {
            progress.check()?;
            if candidate.id != file.id
                && !query.searches_project()
                && !query
                    .seed_files()
                    .iter()
                    .any(|seed| seed.id == candidate.id)
            {
                continue;
            }
            if !query.candidate_text_may_match(&candidate.text) {
                continue;
            }
            for usage in query.in_file(db, &candidate) {
                progress.check()?;
                if usage.is_definition || usage.dynamic || usage.non_code {
                    continue;
                }
                if matches[index].insert((usage.path, usage.range)) {
                    references += 1;
                    if references > limits.references {
                        return Err(Unavailable::BudgetExceeded);
                    }
                }
            }
        }
    }
    progress.check()?;
    Ok(declarations
        .into_iter()
        .zip(matches)
        .map(|((mut lens, _), usages)| {
            lens.usages = usages.len();
            lens
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    fn project(files: &[(&str, &str)]) -> (Analysis, Vec<PathBuf>) {
        let db = Analysis::for_project(Path::new("/proj"));
        let paths = files
            .iter()
            .map(|(name, text)| {
                let path = PathBuf::from(format!("/proj/{name}"));
                db.set_file_text(&path, text);
                path
            })
            .collect();
        (db, paths)
    }
    #[test]
    fn usages_follow_import_aliases_and_ignore_same_names_strings_and_comments() {
        let (db, paths) = project(&[
            ("a.py", "def work():\n    return 1\n"),
            (
                "b.py",
                "from a import work as run\nrun()\nrun()\n# work()\ntext = 'work()'\n",
            ),
            ("c.py", "def work():\n    return 2\nwork()\n"),
        ]);
        let file = db.file_for_path(&paths[0]).unwrap();
        let lenses = usage_lenses(
            &db,
            &file,
            &paths,
            Limits::default(),
            &ProgressReporter::new(),
        )
        .unwrap();
        assert_eq!(lenses.len(), 1);
        assert_eq!(lenses[0].name, "work");
        assert_eq!(lenses[0].usages, 3);
        assert_eq!(lenses[0].line, 0);
    }
    #[test]
    fn methods_resolve_receivers_and_exclude_unknown_dynamic_matches() {
        let (db, paths) = project(&[("a.py", "class Job:\n    def run(self):\n        return 1\nclass Other:\n    def run(self):\n        return 2\nJob().run()\nOther().run()\ndef unknown(worker):\n    worker.run()\n")]);
        let file = db.file_for_path(&paths[0]).unwrap();
        let lenses = usage_lenses(
            &db,
            &file,
            &paths,
            Limits::default(),
            &ProgressReporter::new(),
        )
        .unwrap();
        let run: Vec<_> = lenses.iter().filter(|lens| lens.name == "run").collect();
        assert_eq!(run.len(), 2);
        assert_eq!(run[0].usages, 1);
        assert_eq!(run[1].usages, 1);
    }
    #[test]
    fn cancellation_budget_and_missing_index_candidate_never_publish_partial_counts() {
        let (db, paths) = project(&[("a.py", "def work():\n    return 1\nwork()\n")]);
        let file = db.file_for_path(&paths[0]).unwrap();
        let progress = ProgressReporter::new();
        progress.token().cancel();
        assert_eq!(
            usage_lenses(&db, &file, &paths, Limits::default(), &progress),
            Err(Unavailable::Cancelled)
        );
        assert_eq!(
            usage_lenses(
                &db,
                &file,
                &paths,
                Limits {
                    references: 0,
                    ..Limits::default()
                },
                &ProgressReporter::new()
            ),
            Err(Unavailable::BudgetExceeded)
        );
        let mut missing = paths.clone();
        missing.push(PathBuf::from("/proj/absent.py"));
        assert_eq!(
            usage_lenses(
                &db,
                &file,
                &missing,
                Limits::default(),
                &ProgressReporter::new()
            ),
            Err(Unavailable::SourceUnavailable)
        );
    }
}

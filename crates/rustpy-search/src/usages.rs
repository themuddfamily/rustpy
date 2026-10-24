//! Project usage scans and call-site aggregation. Semantic queries resolve
//! and classify symbols; this owner controls candidate loading and execution.

pub mod results;

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use rayon::prelude::*;
use rustpy_core::{Cancelled, ProgressReporter, TextRange, TextSize};
use rustpy_semantic::calls::{self, CallSite, InheritingClass};
use rustpy_semantic::hierarchy::{self, HierarchyNode};
use rustpy_semantic::infer::Definition;
use rustpy_semantic::types::ClassRef;
use rustpy_semantic::usages::{ReexportQuery, Usage, UsageQuery};
use rustpy_semantic::{Analysis, FileInfo};
use rustpy_syntax::ast::{self, AstNode};

/// Find Usages across the project: the symbol's file plus every project
/// file whose text mentions the name (then verified semantically).
///
/// For callers that wait for the answer (the UI thread: Show Usages, Go to
/// Declaration on a definition, Call Hierarchy, Safe Delete, Change
/// Signature): the files are scanned in parallel in the caller's rayon pool
/// (the global one), never behind the background scans of
/// [`rustpy_core::parallel`].
pub fn find_usages(
    db: &Analysis,
    file: &Arc<FileInfo>,
    offset: TextSize,
    project_files: &[PathBuf],
) -> Vec<Usage> {
    search_usages(db, file, offset, project_files, &ProgressReporter::new()).unwrap_or_default()
}

/// [`find_usages`] as a background task: project files are scanned in
/// parallel in the background scan pool (results keep the order of a
/// sequential scan), the scan is reported to `progress`, and it stops with
/// `Err(Cancelled)` once `progress` is cancelled. Only files whose text
/// mentions the name are parsed.
pub fn find_usages_with_progress(
    db: &Analysis,
    file: &Arc<FileInfo>,
    offset: TextSize,
    project_files: &[PathBuf],
    progress: &ProgressReporter,
) -> Result<Vec<Usage>, Cancelled> {
    rustpy_core::parallel::in_background_pool(|| {
        search_usages(db, file, offset, project_files, progress)
    })
}

/// The search behind [`find_usages`] and [`find_usages_with_progress`], in
/// the current rayon pool.
pub fn search_usages(
    db: &Analysis,
    file: &Arc<FileInfo>,
    offset: TextSize,
    project_files: &[PathBuf],
    progress: &ProgressReporter,
) -> Result<Vec<Usage>, Cancelled> {
    progress.check()?;
    let Some(query) = UsageQuery::prepare(db, file, offset) else {
        return Ok(Vec::new());
    };
    let mut out = query.in_file(db, file);
    let mut seen: HashSet<PathBuf> = HashSet::from([file.path.clone()]);
    for df in query.seed_files() {
        if seen.insert(df.path.clone()) {
            out.extend(query.in_file(db, df));
        }
    }
    if !query.searches_project() {
        return Ok(out);
    }
    let mut callee_names = query.callee_names().to_vec();
    if let Some((origin, class, func)) = query.constructor() {
        callee_names.extend(
            classes_inheriting(db, &origin, &class, &func, project_files)
                .into_iter()
                .map(|class| class.name),
        );
    }
    progress.check()?;
    let candidates: Vec<&PathBuf> = project_files
        .iter()
        .filter(|p| {
            seen.insert((*p).clone()) && p.extension().and_then(|e| e.to_str()) == Some("py")
        })
        .collect();
    let hints: HashSet<_> = query
        .importer_paths(db, &|| progress.is_cancelled())
        .ok_or(Cancelled)?
        .into_iter()
        .collect();
    progress.check()?;
    let total = candidates.len();
    let mut work: Vec<_> = candidates.into_iter().enumerate().collect();
    work.sort_by_key(|(_, path)| !hints.contains(path.as_path()));
    let scanned = AtomicUsize::new(0);
    let mut per_file: Vec<(usize, Vec<Usage>)> = work
        .par_iter()
        .map(|(index, path)| {
            let usages = (|| {
                if progress.is_cancelled() {
                    return Vec::new();
                }
                let done = scanned.fetch_add(1, Ordering::Relaxed) + 1;
                if done % 16 == 1 || done == total {
                    progress.set_step(done, total);
                    progress.set_details(path.display().to_string());
                }
                // Cheap text test before parsing: most files never mention the name.
                let Some(text) = db.current_text(path) else {
                    return Vec::new();
                };
                if !query.candidate_text_may_match(&text) {
                    return Vec::new();
                }
                if !callee_names.is_empty()
                    && !callee_names.iter().any(|c| text.contains(c.as_str()))
                {
                    return Vec::new();
                }
                let Some(other) = db.file_for_path(path) else {
                    return Vec::new();
                };
                query.in_file(db, &other)
            })();
            (*index, usages)
        })
        .collect();
    progress.check()?;
    per_file.sort_by_key(|(index, _)| *index);
    out.extend(per_file.into_iter().flat_map(|(_, usages)| usages));
    Ok(out)
}

/// Other project-file usages that depend on a module-level re-export.
pub fn reexport_usages(db: &Analysis, def: &Definition, project_files: &[PathBuf]) -> Vec<Usage> {
    let Some(query) = ReexportQuery::prepare(db, def) else {
        return Vec::new();
    };
    let mut seen: HashSet<&Path> = HashSet::from([def.file.path.as_path()]);
    let mut out = Vec::new();
    for path in project_files {
        if path.extension().and_then(|e| e.to_str()) != Some("py") || !seen.insert(path.as_path()) {
            continue;
        }
        let Some(file) = db.file_for_path(path) else {
            continue;
        };
        if file.text.contains(query.name()) {
            out.extend(query.in_file(&file));
        }
    }
    out.sort_by(|a, b| {
        a.path
            .cmp(&b.path)
            .then(a.range.start().cmp(&b.range.start()))
    });
    out.dedup_by(|a, b| a.path == b.path && a.range == b.range);
    out
}

/// Subclasses of `class` (transitively, in `file` and `project_files`) that
/// inherit its constructor `func` instead of defining their own: for `class
/// Big(Box): pass`, `Big(1)` calls `Box.__init__`.
fn classes_inheriting(
    db: &Analysis,
    file: &Arc<FileInfo>,
    class: &ast::ClassDef,
    func: &ast::FuncDef,
    project_files: &[PathBuf],
) -> Vec<InheritingClass> {
    let mut files: Vec<Arc<FileInfo>> = vec![file.clone()];
    for p in project_files {
        if p.extension().and_then(|e| e.to_str()) == Some("py") && *p != file.path {
            files.extend(db.file_for_path(p));
        }
    }
    let query = calls::CallQuery::new(db);
    let root = ClassRef {
        file: file.id,
        range: class.syntax().text_range(),
        name: class.name_text().into(),
    };
    let mut seen: HashSet<ClassRef> = HashSet::from([root.clone()]);
    let mut queue = vec![root];
    let mut out = Vec::new();
    while let Some(c) = queue.pop() {
        // Only files that mention the class can derive from it.
        let candidates: Vec<Arc<FileInfo>> = files
            .iter()
            .filter(|f| f.text.contains(c.name.as_str()))
            .cloned()
            .collect();
        for sub in hierarchy::direct_subclasses(db, &c, &candidates) {
            if seen.len() > 1000 || !seen.insert(sub.clone()) {
                continue;
            }
            if let Some(inherited) = query.inherited_constructor(db, file, func, &sub) {
                out.push(inherited);
                queue.push(sub);
            }
        }
    }
    out
}

/// Calls of `func` (defined in `file`) in `file` and `project_files`, in
/// file order. Calls that resolve to another function (an override, a
/// same-named function elsewhere) are left out.
pub fn function_call_sites(
    db: &Analysis,
    file: &Arc<FileInfo>,
    func: &ast::FuncDef,
    project_files: &[PathBuf],
) -> Vec<CallSite> {
    let query = calls::CallQuery::new(db);
    let Some(name_tok) = func.name() else {
        return Vec::new();
    };
    let mut found = find_usages(db, file, name_tok.text_range().start(), project_files);
    // `Class(...)` calls `__init__` / `__new__`, and so does `Sub(...)` for
    // a subclass that inherits it.
    let class_def = func
        .syntax()
        .parent()
        .and_then(|b| b.parent())
        .and_then(ast::ClassDef::cast)
        .filter(|_| matches!(func.name_text().as_str(), "__init__" | "__new__"));
    let mut constructor_names: Vec<String> = Vec::new();
    if let Some(class) = &class_def {
        if let Some(class_tok) = class.name() {
            found.extend(find_usages(
                db,
                file,
                class_tok.text_range().start(),
                project_files,
            ));
            constructor_names.push(class_tok.text().to_string());
        }
        for sub in classes_inheriting(db, file, class, func, project_files) {
            found.extend(find_usages(
                db,
                &sub.file,
                sub.name_range.start(),
                project_files,
            ));
            constructor_names.push(sub.name);
        }
    }
    let mut seen: HashSet<(PathBuf, TextRange)> = HashSet::new();
    let mut out = Vec::new();
    for u in found.iter().filter(|u| !u.is_definition) {
        let Some(f) = (if u.path == file.path {
            Some(file.clone())
        } else {
            db.file_for_path(&u.path)
        }) else {
            continue;
        };
        // Deduplicate by call range as in the original collector; semantic
        // classification does not load or traverse any other project file.
        let Some(site) = query.call_site(file, func, f, u, &constructor_names) else {
            continue;
        };
        if seen.insert((u.path.clone(), site.call.syntax().text_range())) {
            out.push(site);
        }
    }
    out.sort_by(|a, b| {
        a.file.path.cmp(&b.file.path).then(
            a.call
                .syntax()
                .text_range()
                .start()
                .cmp(&b.call.syntax().text_range().start()),
        )
    });
    out
}

/// Incoming calls, in the stable order of the underlying usage search.
pub fn callers(
    db: &Analysis,
    file: &Arc<FileInfo>,
    offset: TextSize,
    project_files: &[PathBuf],
) -> Vec<HierarchyNode> {
    find_usages(db, file, offset, project_files)
        .into_iter()
        .filter_map(|usage| {
            if usage.is_definition {
                return None;
            }
            let other = db.file_for_path(&usage.path)?;
            hierarchy::caller_in_file(&other, &usage)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustpy_semantic::usages::searches_project;

    fn setup(files: &[(&str, &str)]) -> (Analysis, Vec<PathBuf>) {
        let db = Analysis::for_project(Path::new("/proj"));
        let mut paths = Vec::new();
        for (name, src) in files {
            let path = PathBuf::from(format!("/proj/{name}"));
            db.set_file_text(&path, src);
            paths.push(path);
        }
        (db, paths)
    }

    #[test]
    fn usages_follow_closed_files_on_disk() {
        let dir = tempfile::TempDir::with_prefix("rustpy-usages").unwrap();
        let root = dir.path();
        let main = root.join("main.py");
        let other = root.join("other.py");
        std::fs::write(&main, "def helper():\n    return 1\n").unwrap();
        std::fs::write(&other, "from main import helper\n\nhelper()\n").unwrap();
        let db = Analysis::for_project(root);
        let files = vec![main.clone(), other.clone()];
        let f = db.file_for_path(&main).unwrap();
        assert_eq!(find_usages(&db, &f, TextSize::new(4), &files).len(), 3);
        // A call added on disk after the first search is found, at its current offset.
        std::fs::write(
            &other,
            "from main import helper\n\nprint('a longer line first')\nhelper()\nhelper()\n",
        )
        .unwrap();
        let u = find_usages(&db, &f, TextSize::new(4), &files);
        let in_other: Vec<&Usage> = u.iter().filter(|u| u.path == other).collect();
        assert_eq!(in_other.len(), 3, "{u:?}");
        assert!(
            in_other.iter().all(|u| u.line_text.contains("helper")),
            "{u:?}"
        );
    }

    #[test]
    fn project_search_is_ordered_and_parses_only_files_that_mention_the_name() {
        let dir = tempfile::TempDir::with_prefix("rustpy-usages").unwrap();
        let root = dir.path();
        std::fs::write(root.join("main.py"), "def helper():\n    return 1\n").unwrap();
        let mut files = vec![root.join("main.py")];
        for i in 0..40 {
            let p = root.join(format!("m{i:02}.py"));
            let src = if i % 2 == 0 {
                format!("from main import helper\n\nV{i} = helper() + helper()\n")
            } else {
                format!("W{i} = {i}\n")
            };
            std::fs::write(&p, src).unwrap();
            files.push(p);
        }
        // Listed out of order: results follow this order, file by file.
        files.reverse();
        let db = Analysis::for_project(root);
        let f = db.file_for_path(&root.join("main.py")).unwrap();
        let progress = ProgressReporter::new();
        let u = find_usages_with_progress(&db, &f, TextSize::new(4), &files, &progress).unwrap();
        assert_eq!(
            u.len(),
            1 + 20 * 3,
            "the definition, plus an import and two calls per importer"
        );
        assert_eq!(
            u,
            find_usages(&db, &f, TextSize::new(4), &files),
            "deterministic"
        );
        let order: Vec<&PathBuf> = u.iter().map(|u| &u.path).fold(Vec::new(), |mut acc, p| {
            if acc.last() != Some(&p) {
                acc.push(p);
            }
            acc
        });
        let expected: Vec<&PathBuf> = std::iter::once(&files[40])
            .chain(files.iter().filter(|p| {
                std::fs::read_to_string(p).unwrap().contains("helper") && !p.ends_with("main.py")
            }))
            .collect();
        assert_eq!(order, expected);
        assert_eq!(progress.progress().2, Some(1.0));
        // The 20 files without `helper` were read but never parsed into the cache.
        let (cached, _) = db.cached_disk_files();
        assert_eq!(cached, 21, "main.py and the 20 importers");
    }

    #[test]
    fn only_symbols_visible_outside_their_file_search_the_project() {
        let main = "def helper(count, *, scale=1):\n    local = count * scale\n    return local\n\nclass Box:\n    size = 1\n\nhelper(2, scale=3)\n";
        let (db, paths) = setup(&[("main.py", main)]);
        let f = db.file_for_path(&paths[0]).unwrap();
        let at = |needle: &str| TextSize::new(main.find(needle).unwrap() as u32);
        assert!(
            searches_project(&db, &f, at("helper")),
            "module-level function"
        );
        assert!(searches_project(&db, &f, at("Box")), "class");
        assert!(searches_project(&db, &f, at("size")), "class attribute");
        assert!(!searches_project(&db, &f, at("local =")), "local variable");
        assert!(
            searches_project(&db, &f, at("scale=1")),
            "keyword parameter: call sites anywhere name it"
        );
        // Local searches need no project files and match a project-wide search.
        assert_eq!(
            find_usages(&db, &f, at("local ="), &[]),
            find_usages(&db, &f, at("local ="), &paths)
        );
    }

    #[test]
    fn finds_calls_across_files_with_binding() {
        let db = Analysis::for_project(Path::new("/proj"));
        let main = "class Box:\n    def __init__(self, size):\n        self.size = size\n\n    def grow(self, by):\n        self.size += by\n\n\nclass Big(Box):\n    def grow(self, by):\n        pass\n\n\ndef make(n):\n    return Box(n)\n";
        let other = "from main import Box, Big, make\n\n\ndef run(b: Box, big: Big, anything):\n    b.grow(1)\n    Box.grow(b, 2)\n    big.grow(3)\n    anything.grow(4)\n    make(5)\n    return Box(size=6)\n";
        let mp = PathBuf::from("/proj/main.py");
        let op = PathBuf::from("/proj/other.py");
        db.set_file_text(&mp, main);
        db.set_file_text(&op, other);
        let f = db.file_for_path(&mp).unwrap();
        let files = vec![mp.clone(), op.clone()];
        let func_at = |needle: &str| {
            f.parse
                .syntax_node()
                .descendants()
                .filter_map(ast::FuncDef::cast)
                .find(|d| d.syntax().text().to_string().starts_with(needle))
                .unwrap()
        };
        let text = |c: &CallSite| c.call.syntax().text().to_string();
        let grow = function_call_sites(
            &db,
            &f,
            &func_at("def grow(self, by):\n        self"),
            &files,
        );
        let shown: Vec<(String, usize, bool)> = grow
            .iter()
            .map(|c| (text(c), c.implicit, c.unresolved))
            .collect();
        // The override's call is not a call of Box.grow; the untyped receiver is flagged.
        assert_eq!(
            shown,
            vec![
                ("b.grow(1)".to_string(), 1, false),
                ("Box.grow(b, 2)".to_string(), 0, false),
                ("anything.grow(4)".to_string(), 1, true)
            ]
        );
        let init = function_call_sites(&db, &f, &func_at("def __init__"), &files);
        let shown: Vec<(String, usize)> = init.iter().map(|c| (text(c), c.implicit)).collect();
        assert_eq!(
            shown,
            vec![("Box(n)".to_string(), 1), ("Box(size=6)".to_string(), 1)]
        );
        let make = function_call_sites(&db, &f, &func_at("def make"), &files);
        assert_eq!(
            make.iter()
                .map(|c| (text(c), c.implicit, c.line()))
                .collect::<Vec<_>>(),
            vec![("make(5)".to_string(), 0, 9)]
        );
    }

    #[test]
    fn cancelled_search_does_not_return_partial_results() {
        let (db, paths) = setup(&[
            ("main.py", "def helper(): pass\n"),
            ("other.py", "from main import helper\nhelper()\n"),
        ]);
        let file = db.file_for_path(&paths[0]).unwrap();
        let progress = ProgressReporter::new();
        progress.token().cancel();
        assert_eq!(
            find_usages_with_progress(&db, &file, TextSize::new(4), &paths, &progress),
            Err(Cancelled)
        );
        assert_eq!(find_usages(&db, &file, TextSize::new(4), &paths).len(), 3);
    }

    #[test]
    fn dirty_unicode_aliases_keep_input_order_and_duplicate_paths_once() {
        let (db, paths) = setup(&[
            ("main.py", "def café(): pass\n"),
            ("z.py", "from main import café as helper\nhelper()\n"),
            ("a.py", "from main import café\ncafé()\n"),
        ]);
        let file = db.file_for_path(&paths[0]).unwrap();
        db.set_file_text(
            &paths[1],
            "from main import café as helper\nfrom main import café\nprint('😀'); café(); café(); helper()\n",
        );
        let candidates = vec![
            paths[2].clone(),
            paths[1].clone(),
            paths[2].clone(),
            paths[0].clone(),
        ];
        let found = find_usages(&db, &file, TextSize::new(4), &candidates);
        let actual: Vec<_> = found
            .iter()
            .map(|usage| {
                (
                    usage.path.clone(),
                    usage.range,
                    usage.line_text.clone(),
                    usage.dynamic,
                    usage.non_code,
                )
            })
            .collect();
        assert_eq!(
            found.iter().filter(|usage| usage.path == paths[1]).count(),
            4
        );
        assert_eq!(
            found.iter().map(|usage| &usage.path).collect::<Vec<_>>(),
            vec![&paths[0], &paths[2], &paths[2], &paths[1], &paths[1], &paths[1], &paths[1]]
        );
        let background = find_usages_with_progress(
            &db,
            &file,
            TextSize::new(4),
            &candidates,
            &ProgressReporter::new(),
        )
        .unwrap();
        assert_eq!(
            actual,
            background
                .iter()
                .map(|usage| (
                    usage.path.clone(),
                    usage.range,
                    usage.line_text.clone(),
                    usage.dynamic,
                    usage.non_code
                ))
                .collect::<Vec<_>>()
        );
        assert!(found
            .iter()
            .filter(|usage| usage.path == paths[1])
            .any(|usage| usage.line_text.contains("😀")));
    }
    #[test]
    fn constructor_keywords_follow_inherited_classes_and_keep_dynamic_flags() {
        let main = "class Box:\n    def __init__(self, size):\n        self.size = size\n";
        let other = "from main import Box\nclass Big(Box): pass\nclass Bigger(Big): pass\nBigger(size=2)\nBox(size=3)\n";
        let (db, paths) = setup(&[("main.py", main), ("other.py", other)]);
        let file = db.file_for_path(&paths[0]).unwrap();
        let offset = TextSize::new(main.find("size):").unwrap() as u32);
        let found = find_usages(&db, &file, offset, &paths);
        assert_eq!(
            found.iter().filter(|usage| usage.path == paths[1]).count(),
            2,
            "{found:?}"
        );
        assert!(found.iter().all(|usage| !usage.dynamic && !usage.non_code));
        assert_eq!(
            found,
            find_usages_with_progress(&db, &file, offset, &paths, &ProgressReporter::new())
                .unwrap()
        );
    }

    #[test]
    fn cancellation_after_candidate_scan_starts_discards_partial_results() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let main = root.join("main.py");
        std::fs::write(&main, "def helper(): pass\n").unwrap();
        let mut paths = vec![main.clone()];
        // Disk-backed candidates deliberately leave parsing to the scan.
        let text = format!("from main import helper\n{}", "helper()\n".repeat(4000));
        for index in 0..32 {
            let path = root.join(format!("use{index}.py"));
            std::fs::write(&path, &text).unwrap();
            paths.push(path);
        }
        let db = Analysis::for_project(root);
        let file = db.file_for_path(&main).unwrap();
        let progress = ProgressReporter::new();
        std::thread::scope(|scope| {
            let cancel = scope.spawn(|| {
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
                while progress.progress().2.is_none() {
                    assert!(
                        std::time::Instant::now() < deadline,
                        "scan never reported progress"
                    );
                    std::thread::yield_now();
                }
                progress.token().cancel();
            });
            let pool = rayon::ThreadPoolBuilder::new()
                .num_threads(1)
                .build()
                .unwrap();
            assert_eq!(
                pool.install(|| search_usages(&db, &file, TextSize::new(4), &paths, &progress)),
                Err(Cancelled)
            );
            cancel.join().unwrap();
        });
        assert!(progress.is_cancelled());
        assert_eq!(
            find_usages(&db, &file, TextSize::new(4), &[]).len(),
            1,
            "a fresh local scan is still usable"
        );
    }
}

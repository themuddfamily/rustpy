//! Find Usages options and presentation-independent result sessions.
use super::*;
use rustpy_semantic::features;
use rustpy_syntax::SyntaxKind;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub enum Scope {
    #[default]
    Project,
    File(PathBuf),
    Directory(PathBuf),
    OpenFiles(Vec<PathBuf>),
    Custom(Vec<PathBuf>),
    /// A Settings → Scopes selection (predefined or named pattern scope), resolved
    /// by the shared scope compiler into the exact files it admits.
    Named(String, Arc<BTreeSet<PathBuf>>),
}
impl Scope {
    pub fn contains(&self, path: &Path) -> bool {
        match self {
            Self::Project => true,
            Self::File(file) => path == file,
            Self::Directory(dir) => path.starts_with(dir),
            Self::OpenFiles(files) => files.iter().any(|file| path == file),
            Self::Custom(roots) => roots
                .iter()
                .any(|root| path == root || path.starts_with(root)),
            Self::Named(_, files) => files.contains(path),
        }
    }
}
#[derive(Clone, Debug)]
pub struct Options {
    pub usages: bool,
    pub implementing: bool,
    pub overriding: bool,
    pub strings: bool,
    pub comments: bool,
    pub text_occurrences: bool,
    pub dynamic: bool,
    pub skip_single: bool,
    pub scope: Scope,
}
impl Default for Options {
    fn default() -> Self {
        Self {
            usages: true,
            implementing: false,
            overriding: false,
            strings: false,
            comments: false,
            text_occurrences: false,
            dynamic: true,
            skip_single: false,
            scope: Scope::Project,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    Read,
    Write,
    Definition,
    Implementation,
    Override,
    Dynamic,
    String,
    Comment,
    Text,
}
impl Kind {
    pub fn icon(self) -> &'static str {
        match self {
            Self::Read => "R",
            Self::Write => "W",
            Self::Definition => "D",
            Self::Implementation => "I",
            Self::Override => "O",
            Self::Dynamic => "?",
            Self::String => "S",
            Self::Comment => "#",
            Self::Text => "T",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Read => "Read",
            Self::Write => "Write",
            Self::Definition => "Definition",
            Self::Implementation => "Implementation",
            Self::Override => "Override",
            Self::Dynamic => "Dynamic reference",
            Self::String => "String",
            Self::Comment => "Comment",
            Self::Text => "Text occurrence",
        }
    }
}
#[derive(Clone, Debug)]
pub struct Hit {
    pub usage: Usage,
    pub kind: Kind,
    pub module: String,
    pub class: String,
}
impl Hit {
    pub fn from_usage(usage: Usage) -> Self {
        let kind = if usage.dynamic {
            Kind::Dynamic
        } else if usage.non_code {
            Kind::Text
        } else if usage.is_definition {
            Kind::Definition
        } else if usage.is_write {
            Kind::Write
        } else {
            Kind::Read
        };
        Self {
            module: usage
                .path
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
            class: "Top level".into(),
            usage,
            kind,
        }
    }
}

/// Semantic references and explicitly requested non-code candidates are never conflated.
/// Scope is applied to seed files too, not merely to newly loaded project candidates.
pub fn search(
    db: &Analysis,
    file: &Arc<FileInfo>,
    offset: TextSize,
    paths: &[PathBuf],
    options: &Options,
    progress: &ProgressReporter,
) -> Result<Vec<Hit>, Cancelled> {
    progress.check()?;
    let Some(query) = UsageQuery::prepare(db, file, offset) else {
        return Ok(Vec::new());
    };
    let name = query.name().to_owned();
    let candidates: Vec<_> = paths
        .iter()
        .filter(|path| options.scope.contains(path))
        .cloned()
        .collect();
    let mut hits = Vec::new();
    if options.usages {
        hits.extend(
            search_usages(db, file, offset, &candidates, progress)?
                .into_iter()
                .filter(|usage| !usage.is_definition && (options.dynamic || !usage.dynamic))
                .take(10_000)
                .map(Hit::from_usage),
        );
    }
    if options.implementing || options.overriding {
        progress.check()?;
        for location in hierarchy::implementations(db, file, offset, &candidates) {
            let Some(owner) = db.file_for_path(&location.path) else {
                continue;
            };
            if !owner
                .parse
                .syntax_node()
                .text_range()
                .contains_range(location.range)
            {
                continue;
            }
            let kind = if owner
                .parse
                .syntax_node()
                .covering_element(location.range)
                .parent()
                .into_iter()
                .flat_map(|node| node.ancestors())
                .find(|node| matches!(node.kind(), SyntaxKind::FUNC_DEF | SyntaxKind::CLASS_DEF))
                .is_some_and(|node| node.kind() == SyntaxKind::FUNC_DEF)
            {
                Kind::Override
            } else {
                Kind::Implementation
            };
            if (kind == Kind::Override && options.overriding)
                || (kind == Kind::Implementation && options.implementing)
            {
                hits.push(Hit {
                    kind,
                    ..Hit::from_usage(rustpy_semantic::usages::usage_at(
                        &owner,
                        location.range,
                        false,
                        true,
                    ))
                });
            }
        }
    }
    let mut sources = candidates;
    sources.push(file.path.clone());
    sources.sort();
    sources.dedup();
    if options.strings || options.comments || options.text_occurrences || options.dynamic {
        for path in &sources {
            progress.check()?;
            if hits.len() >= 10_000 {
                break;
            }
            if !options.scope.contains(path) {
                continue;
            }
            let Some(source) = db.file_for_path(path) else {
                continue;
            };
            if options.dynamic {
                for usage in dynamic_string_references(db, &source, &name)
                    .into_iter()
                    .take(10_000usize.saturating_sub(hits.len()))
                {
                    hits.push(Hit {
                        kind: Kind::Dynamic,
                        ..Hit::from_usage(usage)
                    });
                }
            }
            if options.strings || options.comments {
                for usage in rustpy_semantic::usages::text_occurrences(&source, &name, None)
                    .into_iter()
                    .take(10_000usize.saturating_sub(hits.len()))
                {
                    let comment = source
                        .parse
                        .syntax_node()
                        .covering_element(usage.range)
                        .kind()
                        == SyntaxKind::COMMENT;
                    if (comment && options.comments) || (!comment && options.strings) {
                        hits.push(Hit {
                            kind: if comment { Kind::Comment } else { Kind::String },
                            ..Hit::from_usage(usage)
                        });
                    }
                }
            }
            if options.text_occurrences {
                for (start, _) in source.text.match_indices(&name) {
                    if hits.len() >= 10_000 {
                        break;
                    }
                    if source.text[..start]
                        .chars()
                        .next_back()
                        .is_some_and(rustpy_core::is_ident_continue)
                        || source.text[start + name.len()..]
                            .chars()
                            .next()
                            .is_some_and(rustpy_core::is_ident_continue)
                    {
                        continue;
                    }
                    let range = TextRange::new(
                        TextSize::new(start as u32),
                        TextSize::new((start + name.len()) as u32),
                    );
                    hits.push(Hit {
                        kind: Kind::Text,
                        ..Hit::from_usage(Usage {
                            non_code: true,
                            ..rustpy_semantic::usages::usage_at(&source, range, false, false)
                        })
                    });
                }
            }
        }
    }
    let mut seen = HashSet::new();
    hits.retain(|hit| {
        options.scope.contains(&hit.usage.path)
            && seen.insert((hit.usage.path.clone(), hit.usage.range))
    });
    hits.truncate(10_000);
    for hit in &mut hits {
        progress.check()?;
        if let Some(owner) = db.file_for_path(&hit.usage.path) {
            hit.module = owner
                .module_name
                .clone()
                .unwrap_or_else(|| hit.module.clone());
            if owner
                .parse
                .syntax_node()
                .text_range()
                .contains_range(hit.usage.range)
            {
                hit.class = owner
                    .parse
                    .syntax_node()
                    .covering_element(hit.usage.range)
                    .parent()
                    .into_iter()
                    .flat_map(|node| node.ancestors())
                    .find_map(ast::ClassDef::cast)
                    .map(|class| class.name_text())
                    .unwrap_or_else(|| "Top level".into());
            }
        }
    }
    hits.sort_by(|a, b| {
        (&a.usage.path, a.usage.range.start(), a.kind).cmp(&(
            &b.usage.path,
            b.usage.range.start(),
            b.kind,
        ))
    });
    Ok(hits)
}

fn dynamic_string_references(db: &Analysis, file: &Arc<FileInfo>, name: &str) -> Vec<Usage> {
    let mut out = Vec::new();
    for call in file
        .parse
        .syntax_node()
        .descendants()
        .take(100_000)
        .filter_map(ast::CallExpr::cast)
    {
        let Some(callee) = call.callee() else {
            continue;
        };
        let Some(token) = callee
            .syntax()
            .descendants_with_tokens()
            .filter_map(|element| element.into_token())
            .filter(|token| token.kind() == SyntaxKind::IDENT)
            .last()
        else {
            continue;
        };
        let canonical = features::definition_at(db, file, token.text_range().start())
            .into_iter()
            .any(|location| {
                if !location.path.starts_with("<typeshed>")
                    || location
                        .path
                        .file_name()
                        .is_none_or(|name| name != "builtins.pyi")
                {
                    return false;
                }
                db.file_for_path(&location.path).is_some_and(|owner| {
                    let range =
                        usize::from(location.range.start())..usize::from(location.range.end());
                    owner.text.get(range).is_some_and(|name| {
                        matches!(name, "getattr" | "setattr" | "hasattr" | "delattr")
                    })
                })
            });
        if !canonical {
            continue;
        }
        let Some(ast::AnyArg::Positional(argument)) =
            call.arg_list().and_then(|list| list.args().nth(1))
        else {
            continue;
        };
        let Some(ast::Expr::String(value)) = argument.expr() else {
            continue;
        };
        let raw = value.syntax().text().to_string();
        if raw
            .chars()
            .take_while(|ch| ch.is_ascii_alphabetic())
            .any(|ch| matches!(ch, 'b' | 'B'))
            || ast::decode_string_literal(&raw) != name
        {
            continue;
        }
        let literal_range = value.syntax().text_range();
        let range = raw
            .find(name)
            .map(|index| {
                let start = literal_range.start() + TextSize::new(index as u32);
                TextRange::new(start, start + TextSize::new(name.len() as u32))
            })
            .unwrap_or(literal_range);
        out.push(Usage {
            dynamic: true,
            non_code: true,
            ..rustpy_semantic::usages::usage_at(file, range, false, false)
        });
        if out.len() == 10_000 {
            break;
        }
    }
    out
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Group {
    UsageType,
    Module,
    Directory,
    #[default]
    File,
    Class,
}
pub struct View<'a> {
    pub filter: &'a str,
    pub show_excluded: bool,
    pub reads: bool,
    pub writes: bool,
    pub dynamic: bool,
    pub merge_lines: bool,
}
impl Default for View<'_> {
    fn default() -> Self {
        Self {
            filter: "",
            show_excluded: false,
            reads: true,
            writes: true,
            dynamic: true,
            merge_lines: false,
        }
    }
}
#[derive(Clone, Debug, Default)]
pub struct Session {
    pub title: String,
    pub hits: Vec<Hit>,
    pub excluded: HashSet<(PathBuf, TextRange)>,
}
impl Session {
    pub fn is_excluded(&self, index: usize) -> bool {
        self.hits.get(index).is_some_and(|hit| {
            self.excluded
                .contains(&(hit.usage.path.clone(), hit.usage.range))
        })
    }
    pub fn set_excluded(&mut self, index: usize, excluded: bool) {
        if let Some(hit) = self.hits.get(index) {
            let key = (hit.usage.path.clone(), hit.usage.range);
            if excluded {
                self.excluded.insert(key);
            } else {
                self.excluded.remove(&key);
            }
        }
    }
    pub fn set_line_excluded(&mut self, path: &Path, line: usize, excluded: bool) {
        let indices: Vec<_> = self
            .hits
            .iter()
            .enumerate()
            .filter(|(_, hit)| hit.usage.path == path && hit.usage.line == line)
            .map(|(index, _)| index)
            .collect();
        for index in indices {
            self.set_excluded(index, excluded);
        }
    }
    pub fn grouped(&self, group: Group, view: View<'_>) -> BTreeMap<String, Vec<usize>> {
        let View {
            filter,
            show_excluded,
            reads,
            writes,
            dynamic,
            merge_lines,
        } = view;
        let mut groups: BTreeMap<String, Vec<usize>> = BTreeMap::new();
        let mut lines = HashSet::new();
        let filter = filter.to_lowercase();
        for (index, hit) in self.hits.iter().enumerate() {
            if (!show_excluded && self.is_excluded(index))
                || (!reads && hit.kind == Kind::Read)
                || (!writes && hit.kind == Kind::Write)
                || (!dynamic && hit.kind == Kind::Dynamic)
            {
                continue;
            }
            if !filter.is_empty()
                && !format!(
                    "{} {} {} {}",
                    hit.usage.path.display(),
                    hit.usage.line_text,
                    hit.module,
                    hit.kind.label()
                )
                .to_lowercase()
                .contains(&filter)
            {
                continue;
            }
            let key = match group {
                Group::UsageType => hit.kind.label().into(),
                Group::Module => hit.module.clone(),
                Group::Directory => hit
                    .usage
                    .path
                    .parent()
                    .unwrap_or(Path::new(""))
                    .display()
                    .to_string(),
                Group::File => hit.usage.path.display().to_string(),
                Group::Class => format!("{}::{}", hit.module, hit.class),
            };
            if merge_lines && !lines.insert((key.clone(), hit.usage.path.clone(), hit.usage.line)) {
                continue;
            }
            groups.entry(key).or_default().push(index);
        }
        groups
    }
    pub fn export_tsv(&self) -> String {
        let mut text = "kind\tfile\tline\tcolumn\texcluded\ttext\n".to_string();
        for (index, hit) in self.hits.iter().enumerate() {
            text.push_str(&format!(
                "{}\t{}\t{}\t{}\t{}\t{}\n",
                hit.kind.label(),
                hit.usage.path.display(),
                hit.usage.line + 1,
                hit.usage.column + 1,
                self.is_excluded(index),
                hit.usage.line_text.replace(['\t', '\r', '\n'], " ")
            ));
        }
        text
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn setup(files: &[(&str, &str)]) -> (Analysis, Vec<PathBuf>) {
        let db = Analysis::for_project(Path::new("/usage-options"));
        let paths = files
            .iter()
            .map(|(name, text)| {
                let path = Path::new("/usage-options").join(name);
                db.set_file_text(&path, text);
                path
            })
            .collect();
        (db, paths)
    }
    #[test]
    fn semantic_options_preserve_identity_and_scope_seed_files_too() {
        let (db, paths) = setup(&[
            ("source.py", "def helper():\n    pass\nhelper()\n"),
            ("use.py", "from source import helper\nhelper()\n"),
            ("shadow.py", "def helper():\n    pass\nhelper()\n"),
        ]);
        let file = db.file_for_path(&paths[0]).unwrap();
        let options = Options {
            scope: Scope::File(paths[1].clone()),
            dynamic: false,
            ..Default::default()
        };
        let hits = search(
            &db,
            &file,
            TextSize::new(4),
            &paths,
            &options,
            &ProgressReporter::new(),
        )
        .unwrap();
        assert!(!hits.is_empty());
        assert!(hits
            .iter()
            .all(|hit| hit.usage.path == paths[1] && !hit.usage.is_definition));
        assert!(hits.iter().any(|hit| hit.usage.line == 1));
        let custom = Scope::Custom(vec![Path::new("/usage-options/sub").into()]);
        assert!(custom.contains(Path::new("/usage-options/sub/file.py")));
        assert!(!custom.contains(Path::new("/usage-options/submarine/file.py")));
    }
    #[test]
    fn strings_comments_and_canonical_dynamic_strings_are_distinct_candidates() {
        let (db, paths) = setup(&[("source.py", "def helper():\n    pass\n# helper\nlabel = 'helper'\ngetattr(object(), 'helper')\n"), ("shadow.py", "def getattr(obj, name):\n    return None\ngetattr(object(), 'helper')\n")]);
        let file = db.file_for_path(&paths[0]).unwrap();
        let options = Options {
            usages: false,
            strings: true,
            comments: true,
            ..Default::default()
        };
        let hits = search(
            &db,
            &file,
            TextSize::new(4),
            &paths,
            &options,
            &ProgressReporter::new(),
        )
        .unwrap();
        assert!(hits
            .iter()
            .any(|hit| hit.kind == Kind::Comment && hit.usage.line == 2));
        assert!(hits
            .iter()
            .any(|hit| hit.kind == Kind::String && hit.usage.line == 3));
        assert!(hits
            .iter()
            .any(|hit| hit.kind == Kind::Dynamic && hit.usage.line == 4 && hit.usage.dynamic));
        assert!(!hits
            .iter()
            .any(|hit| hit.kind == Kind::Dynamic && hit.usage.path == paths[1]));
        let none = Options {
            usages: false,
            dynamic: false,
            ..Default::default()
        };
        assert!(search(
            &db,
            &file,
            TextSize::new(4),
            &paths,
            &none,
            &ProgressReporter::new()
        )
        .unwrap()
        .is_empty());
    }
    #[test]
    fn implementing_and_overriding_options_use_resolved_hierarchy() {
        let source = "class Base:\n    def run(self):\n        pass\nclass Child(Base):\n    def run(self):\n        pass\n";
        let (db, paths) = setup(&[("classes.py", source)]);
        let file = db.file_for_path(&paths[0]).unwrap();
        let classes = Options {
            usages: false,
            dynamic: false,
            implementing: true,
            ..Default::default()
        };
        let hits = search(
            &db,
            &file,
            TextSize::new(source.find("Base").unwrap() as u32),
            &paths,
            &classes,
            &ProgressReporter::new(),
        )
        .unwrap();
        assert!(hits
            .iter()
            .any(|hit| hit.kind == Kind::Implementation && hit.usage.line == 3));
        let methods = Options {
            usages: false,
            dynamic: false,
            overriding: true,
            ..Default::default()
        };
        let hits = search(
            &db,
            &file,
            TextSize::new(source.find("run").unwrap() as u32),
            &paths,
            &methods,
            &ProgressReporter::new(),
        )
        .unwrap();
        assert!(hits
            .iter()
            .any(|hit| hit.kind == Kind::Override && hit.usage.line == 4));
        let cancelled = ProgressReporter::new();
        cancelled.token().cancel();
        assert!(search(&db, &file, TextSize::new(6), &paths, &methods, &cancelled).is_err());
    }
    fn session() -> Session {
        let (db, paths) = setup(&[("sample.py", "value = value + value  # π\n")]);
        let file = db.file_for_path(&paths[0]).unwrap();
        let hits = [0, 8, 16]
            .into_iter()
            .enumerate()
            .map(|(index, offset)| Hit {
                kind: if index == 0 { Kind::Write } else { Kind::Read },
                ..Hit::from_usage(rustpy_semantic::usages::usage_at(
                    &file,
                    TextRange::new(TextSize::new(offset), TextSize::new(offset + 5)),
                    index == 0,
                    false,
                ))
            })
            .collect();
        Session {
            title: "value".into(),
            hits,
            ..Default::default()
        }
    }
    #[test]
    fn grouping_filter_merge_exclude_include_and_export_retain_occurrences() {
        let mut session = session();
        assert_eq!(session.grouped(Group::UsageType, View::default()).len(), 2);
        assert_eq!(
            session
                .grouped(
                    Group::File,
                    View {
                        merge_lines: true,
                        ..Default::default()
                    }
                )
                .values()
                .map(Vec::len)
                .sum::<usize>(),
            1
        );
        assert_eq!(
            session
                .grouped(
                    Group::File,
                    View {
                        reads: false,
                        ..Default::default()
                    }
                )
                .values()
                .map(Vec::len)
                .sum::<usize>(),
            1
        );
        assert!(session
            .grouped(
                Group::Module,
                View {
                    filter: "unmatched",
                    ..Default::default()
                }
            )
            .is_empty());
        let path = session.hits[0].usage.path.clone();
        session.set_line_excluded(&path, 0, true);
        assert!(session.grouped(Group::Class, View::default()).is_empty());
        assert_eq!(
            session
                .grouped(
                    Group::Class,
                    View {
                        show_excluded: true,
                        ..Default::default()
                    }
                )
                .values()
                .map(Vec::len)
                .sum::<usize>(),
            3
        );
        let export = session.export_tsv();
        assert_eq!(export.lines().count(), 4);
        assert!(export.contains("π") && export.contains("\ttrue\t"));
        session.set_line_excluded(&path, 0, false);
        assert!(session.excluded.is_empty());
    }
}

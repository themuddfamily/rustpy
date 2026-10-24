//! Read-only completion state observed while walking the original program.
use super::*;
use std::{cell::RefCell, rc::Rc};
#[derive(Clone)]
enum Site {
    Name(TextSize),
    Gap(TextRange),
}
#[derive(Clone)]
pub(super) struct Observer {
    site: Site,
    names: Vec<SmolStr>,
    result: Rc<RefCell<State>>,
}
impl Observer {
    fn new(file: &FileInfo, offset: TextSize, names: &[SmolStr]) -> Option<Self> {
        let point = usize::from(offset);
        if point > file.text.len()
            || !file.text.is_char_boundary(point)
            || file.text.len() > 1024 * 1024
            || names.len() > 128
            || names.iter().map(|name| name.len()).sum::<usize>() > 16 * 1024
        {
            return None;
        }
        let root = file.parse.syntax_node();
        let token = root.token_at_offset(offset).left_biased()?;
        let node = token.parent()?;
        let name = node
            .ancestors()
            .filter_map(ast::NameExpr::cast)
            .find(|name| name.text_range().contains_inclusive(offset));
        let site = if let Some(name) = name {
            Site::Name(name.text_range().start())
        } else {
            let arguments = node.ancestors().find_map(ast::ArgList::cast);
            if let Some(arguments) = arguments.filter(|a| {
                a.args()
                    .all(|arg| arg.value().is_none_or(|v| v.text_range().end() <= offset))
            }) {
                Site::Gap(arguments.text_range())
            } else {
                let statement = node.ancestors().find_map(ast::Stmt::cast)?;
                match &statement {
                    ast::Stmt::Return(r) if r.value().is_none() => {}
                    ast::Stmt::Assign(a) if a.value().is_none() => {}
                    ast::Stmt::AnnAssign(a)
                        if a.value().is_none()
                            && file.text[usize::from(a.text_range().start())..point]
                                .contains('=') => {}
                    _ => return None,
                }
                Site::Gap(statement.syntax().text_range())
            }
        };
        Some(Self {
            site,
            names: names.to_vec(),
            result: Rc::new(RefCell::new(State::default())),
        })
    }
    pub(super) fn observe(&self, walker: &Walker<'_>, state: &State, node: &SyntaxNode) {
        if !walker.record_reads {
            return;
        }
        let matches = match self.site {
            Site::Name(start) => {
                node.kind() == SyntaxKind::NAME_EXPR && node.text_range().start() == start
            }
            Site::Gap(range) => node.text_range() == range,
        };
        if !matches {
            return;
        }
        let mut result = self.result.borrow_mut();
        for name in &self.names {
            let mut values = walker.reaching_name(name, state).unwrap_or_else(|| {
                vec![Reach {
                    binding: None,
                    preds: Vec::new(),
                }]
            });
            if values.is_empty() {
                values.push(Reach {
                    binding: None,
                    preds: Vec::new(),
                });
            }
            let accumulated = result.entry(name.clone()).or_default();
            for value in values {
                if !accumulated.contains(&value) {
                    accumulated.push(value);
                }
            }
            if accumulated.len() > 16 {
                *accumulated = vec![Reach {
                    binding: None,
                    preds: Vec::new(),
                }];
            }
        }
    }
}
/// Exact original-file authority and epoch required; no default-authority fallback.
pub fn reaching_at_with_db(
    db: &crate::db::Analysis,
    file: &FileInfo,
    scope: ScopeId,
    offset: TextSize,
    names: &[SmolStr],
) -> Option<FxHashMap<SmolStr, Vec<Reach>>> {
    let current = || {
        db.file(file.id)
            .is_some_and(|f| std::ptr::eq(f.as_ref(), file))
    };
    if !current() {
        return None;
    }
    let observer = Observer::new(file, offset, names)?;
    let authority = terminal::Authority::captured(db, file);
    let epoch = db.revision_excluding(&file.path);
    if !current() || !authority.is_current(db) || db.revision_excluding(&file.path) != epoch {
        return None;
    }
    super::analyze_observed(file, scope, authority.clone(), Some(observer.clone()));
    if !current() || !authority.is_current(db) || db.revision_excluding(&file.path) != epoch {
        return None;
    }
    let result = observer.result.borrow().clone();
    Some(result)
}

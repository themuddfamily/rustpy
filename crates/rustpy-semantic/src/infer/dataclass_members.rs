//! Canonical dataclass InitVar member refinement, separate from constructor synthesis.
use super::*;

impl Inferer<'_> {
    fn canonical_dataclasses_member(
        &self,
        file: &Arc<FileInfo>,
        expression: &ast::Expr,
        name: &str,
    ) -> bool {
        self.symbol_origin(file, expression)
            .is_some_and(|(module, member)| module == "dataclasses" && member == name)
            && self.db.module("dataclasses").is_some_and(|module| {
                module.is_stub && module.path == std::path::Path::new("<typeshed>/dataclasses.pyi")
            })
    }

    pub(super) fn refine_dataclass_initvars(
        &self,
        file: &Arc<FileInfo>,
        class: &ast::ClassDef,
        info: &mut ClassInfo,
    ) {
        let canonical = class
            .decorators()
            .filter_map(|decorator| decorator.expr())
            .any(|expression| {
                let callable = match expression.clone().unparen() {
                    ast::Expr::Call(call) => call.callee(),
                    other => Some(other),
                };
                callable.is_some_and(|callable| {
                    self.canonical_dataclasses_member(file, &callable, "dataclass")
                })
            });
        if !canonical {
            return;
        }
        let Some(body) = class.body() else { return };
        let statements = crate::static_cond::flatten_live(&body, file.static_env.as_ref());
        for (index, statement) in statements.iter().enumerate() {
            let ast::Stmt::AnnAssign(assignment) = statement else {
                continue;
            };
            let Some(ast::Expr::Name(target)) = assignment.target() else {
                continue;
            };
            let Some(annotation) = assignment.annotation() else {
                continue;
            };
            let head = match annotation.unparen() {
                ast::Expr::Subscript(subscript) => subscript.object(),
                other => Some(other),
            };
            if !head.is_some_and(|head| self.canonical_dataclasses_member(file, &head, "InitVar")) {
                continue;
            }
            let name = target.name_text();
            // The annotation remains InitVar after a later plain assignment.
            // Dataclasses observes the final class namespace default, not merely
            // the value attached to the annotation statement.
            let mut namespace_value = assignment.value();
            let mut annotation_replaced = false;
            for later in &statements[index + 1..] {
                match later {
                    ast::Stmt::Assign(store) if store.targets().iter().any(|target| matches!(target.clone().unparen(), ast::Expr::Name(target) if target.name_text() == name)) => namespace_value = store.value(),
                    ast::Stmt::AnnAssign(store) if matches!(store.target().map(ast::Expr::unparen), Some(ast::Expr::Name(target)) if target.name_text() == name) => {
                        annotation_replaced = true;
                        break;
                    }
                    ast::Stmt::Del(delete) if delete.targets().iter().any(|target| matches!(target.clone().unparen(), ast::Expr::Name(target) if target.name_text() == name)) => namespace_value = None,
                    _ => {}
                }
            }
            if annotation_replaced {
                continue;
            }
            let effective_default = namespace_value.and_then(|value| {
                if self.canonical_dataclasses_member(file, &value, "MISSING") {
                    return None;
                }
                if let ast::Expr::Call(call) = value.clone().unparen() {
                    if call.callee().is_some_and(|callee| {
                        self.canonical_dataclasses_member(file, &callee, "field")
                    }) {
                        return call
                            .arg_list()
                            .into_iter()
                            .flat_map(|arguments| arguments.keywords().collect::<Vec<_>>())
                            .find(|keyword| keyword.name_text() == "default")
                            .and_then(|keyword| keyword.value())
                            .filter(|default| {
                                !self.canonical_dataclasses_member(file, default, "MISSING")
                            });
                    }
                }
                Some(value)
            });
            if let Some(default) = effective_default {
                if let Some(member) = info.members.get_mut(name.as_str()) {
                    member.kind = MemberKind::ClassVar;
                    member.annotation = None;
                    member.value = Some(default.text_range());
                }
            } else {
                info.members.shift_remove(name.as_str());
            }
            // Reuse the maintained receiver/target/store policy rather than a
            // second scanner. The temporary map omits pseudo-field declarations
            // that otherwise suppress actual method stores in the raw collector.
            let mut actual_stores = info.clone();
            actual_stores.members.clear();
            for statement in &statements {
                let ast::Stmt::FuncDef(method) = statement else {
                    continue;
                };
                let Some(member) = info.members.get(method.name_text().as_str()) else {
                    continue;
                };
                if !matches!(member.kind, MemberKind::Method | MemberKind::Property) {
                    continue;
                }
                let receiver = method
                    .params()
                    .and_then(|parameters| parameters.params().next())
                    .and_then(|parameter| parameter.name())
                    .map(|name| name.text().to_string());
                if let (Some(receiver), Some(method_body)) = (receiver, method.body()) {
                    actual_stores.collect_self_attrs(file, &method_body, &receiver);
                }
            }
            if let Some(store) = actual_stores.members.get(name.as_str()) {
                info.members.insert(name.into(), store.clone());
            }
        }
    }
}

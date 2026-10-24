//! Source/database-backed relationship facts; unresolved targets never become fictitious nodes.
use super::DiagramClass;
use std::{collections::HashMap, path::PathBuf};
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Kind {
    ForeignKey,
    OneToOne,
    ManyToMany,
    Association,
}
#[derive(Clone, Debug)]
pub struct Relation {
    pub source: String,
    pub target: String,
    pub label: String,
    pub cardinality: String,
    pub kind: Kind,
}
#[derive(Clone)]
pub struct ModelFact {
    pub id: String,
    pub app: String,
    pub name: String,
    pub path: PathBuf,
    pub range: rustpy_core::TextRange,
    pub fields: Vec<String>,
    pub bases: Vec<String>,
    pub links: Vec<Link>,
}
#[derive(Clone)]
pub struct Link {
    pub field: String,
    pub target: String,
    pub reverse: Option<String>,
    pub nullable: bool,
    pub kind: Kind,
}
pub fn django(facts: &[ModelFact]) -> (Vec<DiagramClass>, Vec<Relation>, Vec<String>) {
    let classes = facts
        .iter()
        .map(|m| DiagramClass {
            id: m.id.clone(),
            name: format!("{}.{}", m.app, m.name),
            path: m.path.clone(),
            range: m.range,
            bases: m.bases.clone(),
            fields: m.fields.clone(),
            methods: vec![],
        })
        .collect();
    let mut relations = vec![];
    let mut issues = vec![];
    for model in facts {
        for link in &model.links {
            let candidates = facts
                .iter()
                .filter(|other| {
                    if link.target == "self" {
                        other.id == model.id
                    } else if let Some((app, name)) = link.target.rsplit_once('.') {
                        other.app == app && other.name == name
                    } else {
                        other.name == link.target && other.app == model.app
                    }
                })
                .collect::<Vec<_>>();
            if candidates.len() != 1 {
                issues.push(format!(
                    "{}.{} → {}: {}",
                    model.name,
                    link.field,
                    link.target,
                    if candidates.is_empty() {
                        "target is outside this captured project model"
                    } else {
                        "ambiguous target"
                    }
                ));
                continue;
            }
            let target = candidates[0];
            relations.push(Relation {
                source: model.id.clone(),
                target: target.id.clone(),
                label: format!(
                    "{}{}",
                    link.field,
                    link.reverse
                        .as_ref()
                        .map(|r| format!(" / {r}"))
                        .unwrap_or_default()
                ),
                cardinality: match link.kind {
                    Kind::ManyToMany => "* ↔ *".into(),
                    Kind::OneToOne => if link.nullable {
                        "0..1 ↔ 1"
                    } else {
                        "1 ↔ 1"
                    }
                    .into(),
                    _ => if link.nullable {
                        "* → 0..1"
                    } else {
                        "* → 1"
                    }
                    .into(),
                },
                kind: link.kind.clone(),
            });
        }
    }
    (classes, relations, issues)
}
pub fn er(
    source: &str,
    objects: &[rustpy_db::workspace::Object],
) -> (Vec<DiagramClass>, Vec<Relation>, Vec<String>) {
    let ids = objects
        .iter()
        .map(|o| (o.qualified(), format!("{source}/{}", o.qualified())))
        .collect::<HashMap<_, _>>();
    let classes = objects
        .iter()
        .filter(|o| !o.columns.is_empty())
        .map(|o| DiagramClass {
            id: ids[&o.qualified()].clone(),
            name: o.qualified(),
            path: PathBuf::new(),
            range: rustpy_core::TextRange::empty(0.into()),
            bases: vec![],
            fields: o
                .columns
                .iter()
                .map(|c| {
                    format!(
                        "{}{}: {}{}",
                        if c.primary { "PK " } else { "" },
                        c.name,
                        c.data_type,
                        if c.nullable { " ?" } else { "" }
                    )
                })
                .collect(),
            methods: vec![],
        })
        .collect();
    let mut links = vec![];
    let mut issues = vec![];
    for object in objects {
        for key in &object.keys {
            let targets = objects
                .iter()
                .filter(|o| {
                    !o.columns.is_empty()
                        && (o.qualified() == key.target
                            || (o.name == key.target
                                && o.schema == object.schema
                                && o.catalog == object.catalog))
                })
                .collect::<Vec<_>>();
            if targets.len() != 1 {
                issues.push(format!(
                    "{}.{} → {}: unresolved or ambiguous foreign table",
                    object.qualified(),
                    key.column,
                    key.target
                ));
                continue;
            }
            links.push(Relation {
                source: ids[&object.qualified()].clone(),
                target: ids[&targets[0].qualified()].clone(),
                label: format!("{} → {}", key.column, key.target_column),
                cardinality: if object
                    .columns
                    .iter()
                    .any(|c| c.name == key.column && c.nullable)
                {
                    "* → 0..1"
                } else {
                    "* → 1"
                }
                .into(),
                kind: Kind::ForeignKey,
            });
        }
    }
    (classes, links, issues)
}
#[cfg(test)]
mod tests {
    use super::*;
    fn model(app: &str, name: &str) -> ModelFact {
        ModelFact {
            id: format!("{app}/{name}"),
            app: app.into(),
            name: name.into(),
            path: format!("/{app}/models.py").into(),
            range: rustpy_core::TextRange::empty(0.into()),
            fields: vec![],
            bases: vec![],
            links: vec![],
        }
    }
    #[test]
    fn lazy_targets_self_cardinality_and_duplicate_names_are_exact() {
        let mut account = model("billing", "Account");
        account.links.push(Link {
            field: "parent".into(),
            target: "self".into(),
            reverse: Some("children".into()),
            nullable: true,
            kind: Kind::ForeignKey,
        });
        account.links.push(Link {
            field: "owner".into(),
            target: "users.Account".into(),
            reverse: None,
            nullable: false,
            kind: Kind::OneToOne,
        });
        let (_, links, issues) = django(&[
            account,
            model("users", "Account"),
            model("other", "Account"),
        ]);
        assert!(issues.is_empty());
        assert_eq!(links[0].source, links[0].target);
        assert_eq!(links[0].cardinality, "* → 0..1");
        assert_eq!(links[1].target, "users/Account");
    }
    #[test]
    fn absent_models_are_diagnostics_not_placeholder_nodes() {
        let mut fact = model("app", "Entry");
        fact.links.push(Link {
            field: "user".into(),
            target: "auth.User".into(),
            reverse: None,
            nullable: false,
            kind: Kind::ForeignKey,
        });
        let (classes, relations, issues) = django(&[fact]);
        assert_eq!(classes.len(), 1);
        assert!(relations.is_empty());
        assert_eq!(issues.len(), 1);
    }
}

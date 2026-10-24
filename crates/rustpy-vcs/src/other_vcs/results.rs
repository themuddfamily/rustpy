//! Provider result parsing. Every displayed revision, path and relationship comes
//! from provider output; unknown/version-specific fields remain available verbatim.
use super::{Format, Operation};
use crate::provider::Provider;
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};
#[derive(Clone, Debug, Default)]
pub struct Row {
    pub id: String,
    pub label: String,
    pub detail: String,
    pub path: Option<PathBuf>,
    pub revision: Option<String>,
    pub fields: BTreeMap<String, String>,
}
#[derive(Clone, Debug)]
pub struct RevisionNode {
    pub id: String,
    pub file: String,
    pub revision: u32,
    pub change: String,
    pub user: String,
    pub description: String,
}
#[derive(Clone, Debug)]
pub struct RevisionEdge {
    pub from: String,
    pub to: String,
    pub relation: String,
}
#[derive(Clone, Debug, Default)]
pub struct Data {
    pub rows: Vec<Row>,
    pub graph: Vec<RevisionNode>,
    pub edges: Vec<RevisionEdge>,
    pub raw: String,
}
fn local(root: &Path, path: &str) -> PathBuf {
    let path = PathBuf::from(path);
    if path.is_absolute() {
        path
    } else {
        root.join(path)
    }
}
fn child(node: roxmltree::Node<'_, '_>, name: &str) -> String {
    node.children()
        .find(|n| n.has_tag_name(name))
        .and_then(|n| n.text())
        .unwrap_or("")
        .to_string()
}
pub fn tagged(text: &str) -> Vec<BTreeMap<String, String>> {
    let mut records = Vec::new();
    let mut current = BTreeMap::new();
    let mut last = None;
    for line in text.lines() {
        if let Some(value) = line.strip_prefix("... ") {
            let (key, value) = value.split_once(' ').unwrap_or((value, ""));
            if current.contains_key(key) {
                records.push(std::mem::take(&mut current));
            }
            current.insert(key.to_string(), value.to_string());
            last = Some(key.to_string());
        } else if line.trim().is_empty() {
            if !current.is_empty() {
                records.push(std::mem::take(&mut current));
            }
            last = None;
        } else if let Some(key) = &last {
            if let Some(value) = current.get_mut(key) {
                value.push('\n');
                value.push_str(line);
            }
        }
    }
    if !current.is_empty() {
        records.push(current);
    }
    records
}
/// `p4 filelog -l -ztag` separates indexed revisions with blank lines, but
/// emits depotFile only once per file. Those separators are not file boundaries.
fn filelog_records(text: &str) -> anyhow::Result<Vec<BTreeMap<String, String>>> {
    let mut files: Vec<BTreeMap<String, String>> = Vec::new();
    for record in tagged(text) {
        if record.contains_key("depotFile") {
            files.push(record);
        } else {
            let file = files.last_mut().ok_or_else(|| {
                anyhow::anyhow!("Perforce filelog metadata has no depotFile authority")
            })?;
            for (key, value) in record {
                anyhow::ensure!(
                    !file.contains_key(&key),
                    "Perforce filelog repeats an indexed field within one file"
                );
                file.insert(key, value);
            }
        }
    }
    Ok(files)
}
pub fn decode(
    provider: Provider,
    root: &Path,
    format: Format,
    operation: Operation,
    text: &str,
) -> anyhow::Result<Data> {
    let mut data = Data {
        raw: text.to_string(),
        ..Default::default()
    };
    match format {
        Format::TfsStatus | Format::TfsHistory | Format::TfsWorkspaces => {
            return super::tfs::decode(root, format, text);
        }
        Format::HgStatus => {
            for (index, record) in text.split('\0').filter(|s| !s.is_empty()).enumerate() {
                if let Some(source) = record.strip_prefix("  ") {
                    if let Some(last) = data.rows.last_mut() {
                        last.fields.insert("copyFrom".into(), source.into());
                    }
                    continue;
                }
                let Some(path) = record.get(2..) else {
                    continue;
                };
                let status = record.chars().next().unwrap_or('?');
                let mut fields = BTreeMap::new();
                fields.insert("status".into(), status.to_string());
                data.rows.push(Row {
                    id: index.to_string(),
                    label: path.into(),
                    detail: match status {
                        'M' => "Modified",
                        'A' => "Added",
                        'R' => "Removed",
                        '!' => "Missing",
                        '?' => "Untracked",
                        'I' => "Ignored",
                        'C' => "Clean",
                        _ => "Provider status",
                    }
                    .into(),
                    path: Some(local(root, path)),
                    fields,
                    ..Default::default()
                });
            }
        }
        Format::HgJson => {
            if text.trim().is_empty() {
                return Ok(data);
            }
            let values: serde_json::Value = serde_json::from_str(text)?;
            for (index, value) in values
                .as_array()
                .ok_or_else(|| anyhow::anyhow!("Mercurial did not return a JSON array"))?
                .iter()
                .enumerate()
            {
                let mut fields = BTreeMap::new();
                if let Some(object) = value.as_object() {
                    for (name, value) in object {
                        fields.insert(
                            name.clone(),
                            value
                                .as_str()
                                .map(str::to_string)
                                .unwrap_or_else(|| value.to_string()),
                        );
                    }
                }
                let revision = fields.get("node").or_else(|| fields.get("rev")).cloned();
                let label = fields
                    .get("desc")
                    .and_then(|s| s.lines().next())
                    .or_else(|| fields.get("bookmark").map(String::as_str))
                    .or_else(|| fields.get("branch").map(String::as_str))
                    .unwrap_or("Revision")
                    .to_string();
                let detail = [fields.get("user"), fields.get("branch"), revision.as_ref()]
                    .into_iter()
                    .flatten()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(" · ");
                data.rows.push(Row {
                    id: revision.clone().unwrap_or_else(|| index.to_string()),
                    label,
                    detail,
                    revision,
                    fields,
                    ..Default::default()
                });
            }
        }
        Format::SvnStatus
        | Format::SvnLog
        | Format::SvnInfo
        | Format::SvnList
        | Format::SvnProperties => {
            let document = roxmltree::Document::parse(text)?;
            match format {
                Format::SvnStatus => {
                    for entry in document.descendants().filter(|n| n.has_tag_name("entry")) {
                        let path = entry.attribute("path").unwrap_or("");
                        let Some(status) = entry.children().find(|n| n.has_tag_name("wc-status"))
                        else {
                            continue;
                        };
                        let mut fields = BTreeMap::new();
                        for attribute in status.attributes() {
                            fields.insert(attribute.name().into(), attribute.value().into());
                        }
                        if let Some(name) = entry
                            .ancestors()
                            .find(|n| n.has_tag_name("changelist"))
                            .and_then(|n| n.attribute("name"))
                        {
                            fields.insert("changelist".into(), name.into());
                        }
                        data.rows.push(Row {
                            id: path.into(),
                            label: path.into(),
                            detail: fields.get("item").cloned().unwrap_or_default(),
                            path: Some(local(root, path)),
                            revision: fields.get("revision").cloned(),
                            fields,
                        });
                    }
                }
                Format::SvnLog => {
                    for entry in document
                        .descendants()
                        .filter(|n| n.has_tag_name("logentry"))
                    {
                        let revision = entry.attribute("revision").unwrap_or("").to_string();
                        let mut fields = BTreeMap::new();
                        for name in ["author", "date", "msg"] {
                            fields.insert(name.into(), child(entry, name));
                        }
                        let paths = entry
                            .descendants()
                            .filter(|n| n.has_tag_name("path"))
                            .map(|n| {
                                format!(
                                    "{} {}",
                                    n.attribute("action").unwrap_or(""),
                                    n.text().unwrap_or("")
                                )
                            })
                            .collect::<Vec<_>>()
                            .join("\n");
                        fields.insert("paths".into(), paths);
                        data.rows.push(Row {
                            id: revision.clone(),
                            label: fields["msg"].lines().next().unwrap_or("").into(),
                            detail: format!(
                                "r{revision} · {} · {}",
                                fields["author"], fields["date"]
                            ),
                            path: None,
                            revision: Some(revision),
                            fields,
                        });
                    }
                }
                Format::SvnInfo => {
                    for entry in document.descendants().filter(|n| n.has_tag_name("entry")) {
                        let mut fields = BTreeMap::new();
                        for n in entry
                            .descendants()
                            .filter(|n| n.is_element() && n.children().all(|n| !n.is_element()))
                        {
                            if let Some(value) = n.text() {
                                fields.insert(n.tag_name().name().into(), value.into());
                            }
                        }
                        let path = entry.attribute("path").unwrap_or("");
                        data.rows.push(Row {
                            id: path.into(),
                            label: fields.get("url").cloned().unwrap_or_else(|| path.into()),
                            detail: fields.get("root").cloned().unwrap_or_default(),
                            path: Some(local(root, path)),
                            revision: entry.attribute("revision").map(str::to_string),
                            fields,
                        });
                    }
                }
                Format::SvnList => {
                    for entry in document.descendants().filter(|n| n.has_tag_name("entry")) {
                        let mut fields = BTreeMap::new();
                        fields.insert("kind".into(), entry.attribute("kind").unwrap_or("").into());
                        let commit = entry.children().find(|n| n.has_tag_name("commit"));
                        let revision = commit
                            .and_then(|n| n.attribute("revision"))
                            .map(str::to_string);
                        let name = child(entry, "name");
                        data.rows.push(Row {
                            id: name.clone(),
                            label: name,
                            detail: format!(
                                "{} · {}",
                                fields["kind"],
                                commit.map(|n| child(n, "author")).unwrap_or_default()
                            ),
                            path: None,
                            revision,
                            fields,
                        });
                    }
                }
                Format::SvnProperties => {
                    for target in document.descendants().filter(|n| n.has_tag_name("target")) {
                        for property in target.children().filter(|n| n.has_tag_name("property")) {
                            let path = target.attribute("path").unwrap_or("");
                            let name = property.attribute("name").unwrap_or("");
                            let value = property.text().unwrap_or("");
                            let mut fields = BTreeMap::new();
                            fields.insert("value".into(), value.into());
                            fields.insert("name".into(), name.into());
                            data.rows.push(Row {
                                id: format!("{path}:{name}"),
                                label: name.into(),
                                detail: value.into(),
                                path: Some(local(root, path)),
                                fields,
                                ..Default::default()
                            });
                        }
                    }
                }
                _ => unreachable!(),
            }
        }
        Format::P4Tagged | Format::RevisionGraph => {
            let records = if format == Format::RevisionGraph {
                filelog_records(text)?
            } else {
                tagged(text)
            };
            for (index, fields) in records.into_iter().enumerate() {
                if format == Format::RevisionGraph {
                    let file = fields.get("depotFile").cloned().unwrap_or_default();
                    for (key, value) in &fields {
                        let Some(number) = key
                            .strip_prefix("rev")
                            .filter(|s| s.chars().all(|c| c.is_ascii_digit()))
                        else {
                            continue;
                        };
                        let Ok(revision) = value.parse::<u32>() else {
                            continue;
                        };
                        let id = format!("{file}#{revision}");
                        data.graph.push(RevisionNode {
                            id: id.clone(),
                            file: file.clone(),
                            revision,
                            change: fields
                                .get(&format!("change{number}"))
                                .cloned()
                                .unwrap_or_default(),
                            user: fields
                                .get(&format!("user{number}"))
                                .cloned()
                                .unwrap_or_default(),
                            description: fields
                                .get(&format!("desc{number}"))
                                .cloned()
                                .unwrap_or_default(),
                        });
                        for (relation_key, relation) in fields
                            .iter()
                            .filter(|(key, _)| key.starts_with(&format!("how{number},")))
                        {
                            let suffix = relation_key.trim_start_matches("how");
                            let other_file = fields
                                .get(&format!("file{suffix}"))
                                .cloned()
                                .unwrap_or_default();
                            let other_revision = fields
                                .get(&format!("erev{suffix}"))
                                .cloned()
                                .unwrap_or_default();
                            if !other_file.is_empty() {
                                data.edges.push(RevisionEdge {
                                    from: id.clone(),
                                    to: format!("{other_file}{other_revision}"),
                                    relation: relation.clone(),
                                });
                            }
                        }
                    }
                }
                let id = fields
                    .get("depotFile")
                    .or_else(|| fields.get("change"))
                    .or_else(|| fields.get("branch"))
                    .or_else(|| fields.get("Job"))
                    .or_else(|| fields.get("job"))
                    .cloned()
                    .unwrap_or_else(|| index.to_string());
                let label = fields
                    .get("depotFile")
                    .or_else(|| fields.get("desc"))
                    .or_else(|| fields.get("Description"))
                    .or_else(|| fields.get("clientName"))
                    .cloned()
                    .unwrap_or_else(|| id.clone());
                let detail = [
                    fields.get("action"),
                    fields.get("user"),
                    fields.get("change"),
                    fields.get("status"),
                ]
                .into_iter()
                .flatten()
                .cloned()
                .collect::<Vec<_>>()
                .join(" · ");
                let path = fields
                    .get("clientFile")
                    .filter(|path| !path.starts_with("//"))
                    .map(PathBuf::from);
                let revision = fields
                    .get("rev")
                    .or_else(|| fields.get("headRev"))
                    .or_else(|| fields.get("rev0"))
                    .cloned();
                data.rows.push(Row {
                    id,
                    label,
                    detail,
                    path,
                    revision,
                    fields,
                });
            }
            let ids = data
                .graph
                .iter()
                .map(|n| n.id.clone())
                .collect::<std::collections::BTreeSet<_>>();
            for node in &data.graph {
                if node.revision > 1 {
                    let previous = format!("{}#{}", node.file, node.revision - 1);
                    if ids.contains(&previous) {
                        data.edges.push(RevisionEdge {
                            from: node.id.clone(),
                            to: previous,
                            relation: "previous revision".into(),
                        });
                    }
                }
            }
            data.graph.sort_by(|a, b| {
                a.file
                    .cmp(&b.file)
                    .then_with(|| b.revision.cmp(&a.revision))
            });
        }
        Format::Annotation | Format::TimeLapse => {
            for (line, text) in text.lines().enumerate() {
                let revision = if provider == Provider::Perforce {
                    text.split_once(':')
                        .and_then(|(prefix, _)| prefix.trim().parse::<u64>().ok())
                        .map(|n| n.to_string())
                } else if provider == Provider::Subversion {
                    text.split_whitespace()
                        .next()
                        .filter(|s| s.parse::<u64>().is_ok())
                        .map(str::to_string)
                } else {
                    text.split_whitespace().find_map(|word| {
                        word.split_once(':')
                            .filter(|(rev, node)| {
                                rev.parse::<u64>().is_ok()
                                    && node.chars().all(|c| c.is_ascii_hexdigit())
                            })
                            .map(|(_, node)| node.to_string())
                    })
                };
                data.rows.push(Row {
                    id: (line + 1).to_string(),
                    label: text.into(),
                    revision,
                    ..Default::default()
                });
            }
        }
        Format::Text | Format::UnifiedDiff => {}
    }
    let _ = operation;
    Ok(data)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn hg_status_preserves_spaces_and_copy_sources() {
        let data = decode(
            Provider::Mercurial,
            Path::new("/work"),
            Format::HgStatus,
            Operation::Status,
            "A renamed file.py\0  old file.py\0? scratch.py\0",
        )
        .unwrap();
        assert_eq!(data.rows.len(), 2);
        assert_eq!(data.rows[0].fields["copyFrom"], "old file.py");
    }
    #[test]
    fn svn_properties_and_changelist_names_are_real_xml_values() {
        let status = r#"<status><changelist name="fix"><entry path="file.py"><wc-status item="modified" revision="12"/></entry></changelist></status>"#;
        let data = decode(
            Provider::Subversion,
            Path::new("/work"),
            Format::SvnStatus,
            Operation::Status,
            status,
        )
        .unwrap();
        assert_eq!(data.rows[0].fields["changelist"], "fix");
    }
    #[test]
    fn filelog_blank_revision_separators_keep_each_depot_file_and_integration_authority() {
        let raw = "... depotFile //depot/value.txt\n... rev0 2\n... change0 3\n... user0 owned\n... desc0 owned second43\n\n... rev1 1\n... change1 1\n... desc1 owned first42\n\n... depotFile //depot/branch.txt\n... rev0 1\n... change0 4\n... desc0 owned branch\n\n... how0,0 branch from\n... file0,0 //depot/value.txt\n... erev0,0 #2\n\n";
        let generic = tagged(raw);
        assert_eq!(
            generic.len(),
            4,
            "generic blank-record behavior remains intact"
        );
        let graph = decode(
            Provider::Perforce,
            Path::new("/work"),
            Format::RevisionGraph,
            Operation::RevisionGraph,
            raw,
        )
        .unwrap();
        assert_eq!(graph.graph.len(), 3);
        assert!(graph
            .graph
            .iter()
            .any(|node| node.file == "//depot/value.txt"
                && node.revision == 1
                && node.change == "1"
                && node.description == "owned first42"));
        assert!(graph
            .graph
            .iter()
            .any(|node| node.file == "//depot/value.txt"
                && node.revision == 2
                && node.change == "3"));
        assert!(graph
            .graph
            .iter()
            .any(|node| node.file == "//depot/branch.txt"
                && node.revision == 1
                && node.change == "4"));
        assert!(graph
            .edges
            .iter()
            .any(|edge| edge.from == "//depot/value.txt#2"
                && edge.to == "//depot/value.txt#1"
                && edge.relation == "previous revision"));
        assert!(graph
            .edges
            .iter()
            .any(|edge| edge.from == "//depot/branch.txt#1"
                && edge.to == "//depot/value.txt#2"
                && edge.relation == "branch from"));
        assert_eq!(graph.raw, raw);
        assert!(filelog_records("... rev0 1\n... change0 1\n").is_err());
        assert!(
            filelog_records("... depotFile //depot/value.txt\n... rev0 2\n\n... rev0 1\n").is_err()
        );
    }
    #[test]
    fn p4_revision_graph_retains_integration_edges() {
        let input="... depotFile //depot/main.py\n... rev0 3\n... change0 51\n... how0,0 merge from\n... file0,0 //depot/branch.py\n... erev0,0 #2\n";
        let data = decode(
            Provider::Perforce,
            Path::new("/work"),
            Format::RevisionGraph,
            Operation::RevisionGraph,
            input,
        )
        .unwrap();
        assert_eq!(data.graph[0].revision, 3);
        assert_eq!(data.edges[0].to, "//depot/branch.py#2");
    }
}

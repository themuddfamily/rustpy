use rustpy_db::QueryResult;
#[derive(Clone)]
struct Node {
    id: String,
    parent: Option<String>,
    label: String,
}
pub(super) fn show(ui: &mut egui::Ui, result: &QueryResult) {
    let mut nodes = vec![];
    let col = |name: &str| {
        result
            .columns
            .iter()
            .position(|c| c.eq_ignore_ascii_case(name))
    };
    if let (Some(id), Some(parent), Some(detail)) = (col("id"), col("parent"), col("detail")) {
        for row in &result.rows {
            if let (Some(id), Some(parent), Some(detail)) =
                (row.get(id), row.get(parent), row.get(detail))
            {
                nodes.push(Node {
                    id: id.display(),
                    parent: Some(parent.display()),
                    label: detail.display(),
                });
            }
        }
    } else {
        for row in &result.rows {
            for value in row {
                let text = value.display();
                if let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) {
                    json_nodes(&json, None, &mut nodes);
                } else if text.contains("ShowPlanXML") {
                    if let Ok(doc) = roxmltree::Document::parse(&text) {
                        for operation in
                            doc.descendants().filter(|n| n.tag_name().name() == "RelOp")
                        {
                            let id = operation.attribute("NodeId").unwrap_or("").to_string();
                            let parent = operation
                                .ancestors()
                                .skip(1)
                                .find(|n| n.tag_name().name() == "RelOp")
                                .and_then(|n| n.attribute("NodeId"))
                                .map(str::to_string);
                            nodes.push(Node {
                                id,
                                parent,
                                label: format!(
                                    "{} · rows {} · cost {}",
                                    operation.attribute("PhysicalOp").unwrap_or(""),
                                    operation.attribute("EstimateRows").unwrap_or("?"),
                                    operation
                                        .attribute("EstimatedTotalSubtreeCost")
                                        .unwrap_or("?")
                                ),
                            });
                        }
                    }
                } else {
                    for line in text.lines() {
                        nodes.push(Node {
                            id: nodes.len().to_string(),
                            parent: None,
                            label: line.into(),
                        });
                    }
                }
            }
        }
    }
    if nodes.is_empty() {
        return;
    }
    egui::CollapsingHeader::new("Query plan viewer — operator tree")
        .default_open(true)
        .show(ui, |ui| {
            let roots = nodes
                .iter()
                .filter(|n| {
                    n.parent
                        .as_ref()
                        .is_none_or(|p| !nodes.iter().any(|n| &n.id == p))
                })
                .collect::<Vec<_>>();
            for root in roots {
                draw(ui, root, &nodes, 0);
            }
        });
}
fn draw(ui: &mut egui::Ui, node: &Node, nodes: &[Node], depth: usize) {
    if depth > 64 {
        return;
    }
    egui::CollapsingHeader::new(&node.label)
        .id_salt((&node.id, depth))
        .default_open(true)
        .show(ui, |ui| {
            for child in nodes
                .iter()
                .filter(|n| n.parent.as_ref() == Some(&node.id) && n.id != node.id)
            {
                draw(ui, child, nodes, depth + 1);
            }
        });
}
fn json_nodes(value: &serde_json::Value, parent: Option<String>, nodes: &mut Vec<Node>) {
    if nodes.len() > 10000 {
        return;
    }
    match value {
        serde_json::Value::Array(values) => {
            for value in values {
                json_nodes(value, parent.clone(), nodes)
            }
        }
        serde_json::Value::Object(map) => {
            let next = if let Some(kind) = map.get("Node Type").or_else(|| map.get("name")) {
                let id = nodes.len().to_string();
                nodes.push(Node {
                    id: id.clone(),
                    parent,
                    label: format!(
                        "{} · rows {} · cost {}",
                        kind.as_str().unwrap_or("Operator"),
                        map.get("Plan Rows")
                            .map(ValueLike::text)
                            .unwrap_or_default(),
                        map.get("Total Cost")
                            .map(ValueLike::text)
                            .unwrap_or_default()
                    ),
                });
                Some(id)
            } else {
                parent
            };
            for (value_name, value) in map {
                if ["Plan", "Plans", "children", "query_block", "nested_loop"]
                    .contains(&value_name.as_str())
                {
                    json_nodes(value, next.clone(), nodes);
                }
            }
        }
        _ => {}
    }
}
trait ValueLike {
    fn text(&self) -> String;
}
impl ValueLike for serde_json::Value {
    fn text(&self) -> String {
        self.to_string()
    }
}

use super::*;
use models::{Kind, Link, ModelFact};
use std::{
    collections::hash_map::DefaultHasher,
    hash::{Hash, Hasher},
    path::Path,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    },
};
#[derive(Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Layout {
    #[default]
    Layered,
    Grid,
    Circle,
}
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Preferences {
    pub layout: Layout,
    pub labels: bool,
    pub relations: bool,
    pub fields: bool,
    pub methods: bool,
    pub dependencies: bool,
    pub implementations: bool,
    pub search: String,
    pub export_path: String,
    pub horizontal: bool,
    pub positions: HashMap<String, (f32, f32)>,
}
impl Default for Preferences {
    fn default() -> Self {
        Self {
            layout: Layout::Layered,
            labels: true,
            relations: true,
            fields: true,
            methods: true,
            dependencies: true,
            implementations: true,
            search: String::new(),
            export_path: String::new(),
            horizontal: false,
            positions: HashMap::new(),
        }
    }
}
#[derive(Default)]
pub struct Workspace {
    pub root: Option<PathBuf>,
    pub relations: Vec<models::Relation>,
    pub issues: Vec<String>,
    pub fingerprints: HashMap<PathBuf, u64>,
    pub er_source: Option<String>,
    pub er_nodes: HashMap<String, String>,
    pub preferences: Preferences,
    pub job: Option<Job>,
    pub request: Option<u8>,
}
static NEXT_CAPTURE: AtomicU64 = AtomicU64::new(1);
pub struct Job {
    pub id: u64,
    pub root: PathBuf,
    pub cancelled: Arc<AtomicBool>,
    pub receiver: std::sync::mpsc::Receiver<Capture>,
    pub thread: std::thread::JoinHandle<()>,
}
pub struct Capture {
    pub facts: Vec<ModelFact>,
    pub fingerprints: HashMap<PathBuf, u64>,
    pub truncated: bool,
}
pub fn fingerprint(source: &str) -> u64 {
    let mut hash = DefaultHasher::new();
    source.hash(&mut hash);
    hash.finish()
}
impl Drop for Job {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Release);
    }
}
impl DiagramPanel {
    pub(crate) fn project_context(&mut self, root: &Path) {
        if self.workspace.root.as_deref() == Some(root) {
            return;
        }
        if let Some(job) = &self.workspace.job {
            job.cancelled.store(true, Ordering::Release);
        }
        self.classes.clear();
        self.workspace.relations.clear();
        self.workspace.fingerprints.clear();
        self.workspace.er_source = None;
        self.workspace.er_nodes.clear();
        self.workspace.root = Some(root.into());
        self.workspace.preferences = std::fs::read(root.join(".rustpy/diagrams.json"))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
    }
    pub(crate) fn capture_sources(&mut self, analysis: &rustpy_semantic::Analysis) {
        self.workspace.fingerprints = self
            .classes
            .iter()
            .filter_map(|c| {
                analysis
                    .file_for_path(&c.path)
                    .map(|f| (c.path.clone(), fingerprint(&f.text)))
            })
            .collect();
    }
    pub(crate) fn navigation_current(&self, path: &Path, text: &str) -> bool {
        self.workspace
            .fingerprints
            .get(path)
            .is_none_or(|stamp| *stamp == fingerprint(text))
    }
    pub(crate) fn request_django(
        &mut self,
        root: PathBuf,
        analysis: Arc<rustpy_semantic::Analysis>,
        paths: Vec<PathBuf>,
    ) {
        self.project_context(&root);
        if self.workspace.job.is_some() {
            self.workspace.issues =
                vec!["Wait for or cancel the current captured model job before refreshing".into()];
            return;
        }
        self.workspace.issues.clear();
        let (cancelled, receiver, thread) = {
            let token = Arc::new(AtomicBool::new(false));
            let cancel = token.clone();
            let (tx, rx) = std::sync::mpsc::sync_channel(1);
            let project = root.clone();
            let thread = std::thread::Builder::new()
                .name("diagram-model-capture".into())
                .spawn(move || {
                    let mut facts = vec![];
                    let mut fingerprints = HashMap::new();
                    let mut truncated = paths.len() > 2048;
                    for path in paths.into_iter().take(2048) {
                        if cancel.load(Ordering::Acquire) {
                            return;
                        }
                        let Some(file) = analysis.file_for_path(&path) else {
                            continue;
                        };
                        if file.text.len() > 2 * 1024 * 1024 {
                            truncated = true;
                            continue;
                        }
                        fingerprints.insert(path.clone(), fingerprint(&file.text));
                        for model in
                            rustpy_semantic::frameworks::models_in_analysis(&analysis, &file)
                                .into_iter()
                                .filter(|m| m.family == rustpy_semantic::frameworks::Family::Django)
                        {
                            let parts = path
                                .strip_prefix(&project)
                                .unwrap_or(&path)
                                .components()
                                .map(|c| c.as_os_str().to_string_lossy().to_string())
                                .collect::<Vec<_>>();
                            let app = parts
                                .iter()
                                .position(|p| p == "models" || p == "models.py")
                                .and_then(|i| i.checked_sub(1))
                                .and_then(|i| parts.get(i))
                                .cloned()
                                .unwrap_or_else(|| "app".into());
                            let mut links = vec![];
                            for field in &model.fields {
                                if let Some(target) = &field.related {
                                    let (kind, nullable) = relation_metadata(&file, field.range);
                                    links.push(Link {
                                        field: field.name.clone(),
                                        target: target.clone(),
                                        reverse: field.reverse.clone(),
                                        nullable,
                                        kind,
                                    });
                                }
                            }
                            let inf = rustpy_semantic::infer::Inferer::new(&analysis);
                            let mut bases = vec![];
                            if let Some(info) = inf.class_info(&model.class) {
                                use rustpy_syntax::ast::{self, AstNode};
                                for range in &info.base_exprs {
                                    if let Some(expression) = file
                                        .parse
                                        .syntax_node()
                                        .descendants()
                                        .filter_map(ast::Expr::cast)
                                        .find(|e| e.syntax().text_range() == *range)
                                    {
                                        if let rustpy_semantic::types::Type::Class(base, _) =
                                            inf.type_of_expr(&file, &expression)
                                        {
                                            if let Some(source) = analysis.file(base.file) {
                                                bases.push(format!(
                                                    "{}:{}",
                                                    source.path.display(),
                                                    usize::from(base.range.start())
                                                ));
                                            }
                                        }
                                    }
                                }
                            }
                            facts.push(ModelFact {
                                id: format!(
                                    "{}:{}",
                                    path.display(),
                                    usize::from(model.class.range.start())
                                ),
                                app,
                                name: model.class.name.to_string(),
                                path: path.clone(),
                                range: model.class.range,
                                bases,
                                fields: model
                                    .fields
                                    .iter()
                                    .map(|f| format!("{}: {}", f.name, f.scalar))
                                    .collect(),
                                links,
                            });
                            if facts.len() >= 2000 {
                                truncated = true;
                                break;
                            }
                        }
                        if facts.len() >= 2000 {
                            break;
                        }
                    }
                    let _ = tx.send(Capture {
                        facts,
                        fingerprints,
                        truncated,
                    });
                });
            let Ok(thread) = thread else {
                self.workspace.issues = vec!["Unable to start model capture worker".into()];
                return;
            };
            (token, rx, thread)
        };
        self.workspace.job = Some(Job {
            id: NEXT_CAPTURE.fetch_add(1, Ordering::Relaxed),
            root,
            cancelled,
            receiver,
            thread,
        });
    }
    pub(crate) fn set_er(&mut self, source: String, objects: Vec<rustpy_db::workspace::Object>) {
        let (classes, relations, issues) = models::er(&source, &objects);
        self.set("Database ER — captured introspection".into(), classes);
        self.workspace.relations = relations;
        self.workspace.issues = issues;
        self.workspace.er_nodes = objects
            .iter()
            .map(|o| {
                (
                    format!("{source}/{}", o.qualified()),
                    serde_json::to_string(o).unwrap_or_default(),
                )
            })
            .collect();
        self.workspace.er_source = Some(source);
        self.apply_layout();
        self.restore_positions();
    }
    pub(crate) fn capture_owner(&self) -> Option<u64> {
        self.workspace.job.as_ref().map(|job| job.id)
    }
    pub(crate) fn cancel_capture(&mut self) {
        if let Some(job) = &self.workspace.job {
            job.cancelled.store(true, Ordering::Release);
        }
    }
    pub(crate) fn poll_capture(&mut self) -> bool {
        // Observe termination first: a send racing a later is_finished check must
        // not lose the result when the handle is retired on this frame.
        let finished = self
            .workspace
            .job
            .as_ref()
            .is_some_and(|job| job.thread.is_finished());
        let result = self
            .workspace
            .job
            .as_ref()
            .and_then(|j| j.receiver.try_recv().ok());
        if let Some(capture) = result {
            let current = self.workspace.job.as_ref().is_some_and(|j| {
                self.workspace.root.as_ref() == Some(&j.root)
                    && !j.cancelled.load(Ordering::Acquire)
            });
            if current {
                let (classes, relations, mut issues) = models::django(&capture.facts);
                if capture.truncated {
                    issues.push(
                        "Bounded capture truncated at 2048 files / 2000 models / 2 MiB per source"
                            .into(),
                    );
                }
                self.set("Django models — captured project sources".into(), classes);
                self.workspace.relations = relations;
                self.workspace.issues = issues;
                self.workspace.fingerprints = capture.fingerprints;
                self.apply_layout();
                self.restore_positions();
            }
        }
        if finished {
            self.workspace.job = None;
        }
        self.workspace.job.is_some()
    }
    pub(super) fn workspace_toolbar(&mut self, ui: &mut egui::Ui) {
        self.poll_capture();
        ui.horizontal_wrapped(|ui| {
            for (label, request) in [
                ("UML from active file", 0),
                ("Django project models", 1),
                ("ER from selected database", 2),
            ] {
                if ui.button(crate::i18n::tr_static(label)).clicked() {
                    self.workspace.request = Some(request);
                }
            }
            if self.workspace.job.is_some() {
                crate::a11y::spinner(ui);
                if ui.button("Cancel capture").clicked() {
                    if let Some(job) = &self.workspace.job {
                        job.cancelled.store(true, Ordering::Release);
                    }
                }
                ui.ctx()
                    .request_repaint_after(std::time::Duration::from_millis(80));
            }
        });
        ui.horizontal_wrapped(|ui| {
            for (label, layout) in [
                ("Layered", Layout::Layered),
                ("Grid", Layout::Grid),
                ("Circle", Layout::Circle),
            ] {
                if ui
                    .selectable_value(
                        &mut self.workspace.preferences.layout,
                        layout,
                        crate::i18n::tr_static(label),
                    )
                    .clicked()
                {
                    self.apply_layout();
                }
            }
            if ui
                .checkbox(&mut self.workspace.preferences.horizontal, "Horizontal")
                .changed()
            {
                self.apply_layout();
            }
            ui.checkbox(
                &mut self.workspace.preferences.relations,
                "Model / ER relations",
            );
            ui.checkbox(
                &mut self.workspace.preferences.labels,
                "Relationship labels",
            );
            ui.text_edit_singleline(&mut self.workspace.preferences.search);
            if ui.button("Save diagram settings/layout").clicked() {
                self.workspace.preferences.fields = self.show_fields;
                self.workspace.preferences.methods = self.show_methods;
                self.workspace.preferences.dependencies = self.show_dependencies;
                self.workspace.preferences.implementations = self.show_implementations;
                self.workspace.preferences.positions = self
                    .positions
                    .iter()
                    .map(|(id, p)| (id.clone(), (p.x, p.y)))
                    .collect();
                if let Some(root) = &self.workspace.root {
                    let path = root.join(".rustpy/diagrams.json");
                    let _ = std::fs::create_dir_all(path.parent().unwrap());
                    match serde_json::to_vec_pretty(&self.workspace.preferences)
                        .map_err(|e| e.to_string())
                        .and_then(|b| std::fs::write(path, b).map_err(|e| e.to_string()))
                    {
                        Ok(()) => self.export_message = Some("Diagram settings saved".into()),
                        Err(e) => self.export_message = Some(e),
                    }
                }
            }
        });
        for issue in &self.workspace.issues {
            ui.colored_label(egui::Color32::YELLOW, issue);
        }
    }
    pub(super) fn apply_layout(&mut self) {
        match self.workspace.preferences.layout {
            Layout::Layered => {
                let mut remaining = self
                    .classes
                    .iter()
                    .map(|c| c.id.clone())
                    .collect::<std::collections::BTreeSet<_>>();
                let mut depth = HashMap::<String, usize>::new();
                for _ in 0..self.classes.len() {
                    let ready =
                        remaining
                            .iter()
                            .filter(|id| {
                                self.workspace
                                    .relations
                                    .iter()
                                    .filter(|r| &r.source == *id && r.source != r.target)
                                    .all(|r| !remaining.contains(&r.target))
                                    && self.classes.iter().find(|c| &c.id == *id).is_none_or(|c| {
                                        c.bases.iter().all(|b| !remaining.contains(b))
                                    })
                            })
                            .cloned()
                            .collect::<Vec<_>>();
                    if ready.is_empty() {
                        break;
                    }
                    for id in ready {
                        let level = self
                            .workspace
                            .relations
                            .iter()
                            .filter(|r| r.source == id)
                            .filter_map(|r| depth.get(&r.target))
                            .copied()
                            .chain(
                                self.classes
                                    .iter()
                                    .find(|c| c.id == id)
                                    .into_iter()
                                    .flat_map(|c| c.bases.iter())
                                    .filter_map(|b| depth.get(b))
                                    .copied(),
                            )
                            .max()
                            .map(|n| n + 1)
                            .unwrap_or(0);
                        remaining.remove(&id);
                        depth.insert(id, level);
                    }
                }
                let mut layers = std::collections::BTreeMap::<usize, Vec<String>>::new();
                for c in &self.classes {
                    layers
                        .entry(depth.get(&c.id).copied().unwrap_or(0))
                        .or_default()
                        .push(c.id.clone());
                }
                for (level, mut ids) in layers {
                    ids.sort();
                    for (index, id) in ids.into_iter().enumerate() {
                        self.positions
                            .insert(id, Pos2::new(index as f32 * 280.0, level as f32 * 380.0));
                    }
                }
            }
            Layout::Grid => {
                let columns = (self.classes.len() as f32).sqrt().ceil().max(1.0) as usize;
                for (i, c) in self.classes.iter().enumerate() {
                    self.positions.insert(
                        c.id.clone(),
                        Pos2::new((i % columns) as f32 * 280.0, (i / columns) as f32 * 340.0),
                    );
                }
            }
            Layout::Circle => {
                let total = self.classes.len().max(1) as f32;
                let radius = (total * 45.0).max(250.0);
                for (i, c) in self.classes.iter().enumerate() {
                    let angle = std::f32::consts::TAU * i as f32 / total;
                    self.positions.insert(
                        c.id.clone(),
                        Pos2::new(radius + radius * angle.cos(), radius + radius * angle.sin()),
                    );
                }
            }
        }
        if self.workspace.preferences.horizontal {
            for position in self.positions.values_mut() {
                std::mem::swap(&mut position.x, &mut position.y);
            }
        }
    }
    pub(super) fn navigation_event(&self, index: usize) -> DiagramEvent {
        let class = &self.classes[index];
        if let (Some(source), Some(object)) = (
            &self.workspace.er_source,
            self.workspace.er_nodes.get(&class.id),
        ) {
            DiagramEvent::DatabaseObject(source.clone(), object.clone())
        } else {
            DiagramEvent::Navigate(class.path.clone(), class.range)
        }
    }
}
impl DiagramPanel {
    pub(crate) fn take_request(&mut self) -> Option<u8> {
        self.workspace.request.take()
    }
    pub fn graphviz(&self) -> String {
        let quote = |s: &str| serde_json::to_string(s).unwrap();
        let mut graph = String::from("digraph diagram {\n");
        for class in &self.classes {
            graph.push_str(&format!(
                "{} [shape=box,label={}];\n",
                quote(&class.id),
                quote(&format!(
                    "{}\n{}\n{}",
                    class.name,
                    if self.show_members && self.show_fields {
                        class.fields.join("\n")
                    } else {
                        String::new()
                    },
                    if self.show_members && self.show_methods {
                        class.methods.join("\n")
                    } else {
                        String::new()
                    }
                ))
            ));
            for base in &class.bases {
                if !self
                    .implementations
                    .contains(&(class.id.clone(), base.clone()))
                    || self.show_implementations
                {
                    graph.push_str(&format!(
                        "{} -> {} [arrowhead=empty];\n",
                        quote(&class.id),
                        quote(base)
                    ));
                }
            }
        }
        if self.show_dependencies {
            for (source, target) in &self.dependencies {
                graph.push_str(&format!(
                    "{} -> {} [style=dashed];\n",
                    quote(source),
                    quote(target)
                ));
            }
        }
        for link in self
            .workspace
            .relations
            .iter()
            .filter(|_| self.workspace.preferences.relations)
        {
            graph.push_str(&format!(
                "{} -> {} [label={}];\n",
                quote(&link.source),
                quote(&link.target),
                quote(&format!("{} [{}]", link.label, link.cardinality))
            ));
        }
        graph.push_str("}\n");
        graph
    }
}
fn relation_metadata(
    file: &rustpy_semantic::FileInfo,
    range: rustpy_core::TextRange,
) -> (Kind, bool) {
    use rustpy_syntax::ast::{self, AstNode};
    for statement in file
        .parse
        .syntax_node()
        .descendants()
        .filter_map(ast::Stmt::cast)
    {
        let (target, value) = match statement {
            ast::Stmt::Assign(a) => (a.targets().into_iter().next(), a.value()),
            ast::Stmt::AnnAssign(a) => (a.target(), a.value()),
            _ => continue,
        };
        if !target.is_some_and(|t| t.syntax().text_range() == range) {
            continue;
        }
        let Some(ast::Expr::Call(call)) = value else {
            continue;
        };
        let origin = call
            .callee()
            .and_then(|expr| rustpy_semantic::frameworks::origin(file, &expr))
            .unwrap_or_default();
        let kind = match origin.rsplit('.').next().unwrap_or("") {
            "ManyToManyField" => Kind::ManyToMany,
            "OneToOneField" => Kind::OneToOne,
            _ => Kind::ForeignKey,
        };
        let nullable = call
            .arg_list()
            .and_then(|args| {
                args.keywords()
                    .find(|k| k.name_text() == "null")
                    .and_then(|k| k.value())
            })
            .is_some_and(|expr| expr.syntax().text().to_string().trim() == "True");
        return (kind, nullable);
    }
    (Kind::Association, false)
}

impl DiagramPanel {
    fn restore_positions(&mut self) {
        for (id, (x, y)) in &self.workspace.preferences.positions {
            if self.classes.iter().any(|c| &c.id == id) && x.is_finite() && y.is_finite() {
                self.positions.insert(id.clone(), Pos2::new(*x, *y));
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn class(id: &str) -> DiagramClass {
        DiagramClass {
            id: id.into(),
            name: id.into(),
            path: format!("/{id}.py").into(),
            range: rustpy_core::TextRange::empty(0.into()),
            bases: vec![],
            fields: vec!["name: str".into()],
            methods: vec![],
        }
    }
    #[test]
    fn cyclic_relations_have_finite_nonoverlapping_layout_and_exports() {
        let mut panel = DiagramPanel::default();
        panel.set("Cycle <test>".into(), vec![class("left"), class("right")]);
        panel.workspace.relations = vec![
            models::Relation {
                source: "left".into(),
                target: "right".into(),
                label: "owner <name>".into(),
                cardinality: "* → 1".into(),
                kind: Kind::ForeignKey,
            },
            models::Relation {
                source: "right".into(),
                target: "left".into(),
                label: "back".into(),
                cardinality: "1 → 1".into(),
                kind: Kind::OneToOne,
            },
        ];
        panel.workspace.preferences.labels = true;
        panel.apply_layout();
        assert_ne!(panel.positions["left"], panel.positions["right"]);
        assert!(panel.svg_image().contains("owner &lt;name&gt;"));
        assert!(panel.graphviz().contains("owner <name>"));
    }
    #[test]
    fn captured_navigation_refuses_changed_source() {
        let mut panel = DiagramPanel::default();
        panel
            .workspace
            .fingerprints
            .insert("/model.py".into(), fingerprint("class Model: pass"));
        assert!(panel.navigation_current(Path::new("/model.py"), "class Model: pass"));
        assert!(!panel.navigation_current(Path::new("/model.py"), "\nclass Model: pass"));
    }
    #[test]
    fn database_graph_keeps_exact_source_identity_and_composite_targets() {
        let objects=rustpy_db::schema::parse_ddl("CREATE TABLE customer(id INTEGER PRIMARY KEY);CREATE TABLE orders(id INTEGER PRIMARY KEY,customer_id INTEGER);");
        let mut objects = objects;
        objects[1].keys.push(rustpy_db::workspace::ForeignKey {
            column: "customer_id".into(),
            target: "customer".into(),
            target_column: "id".into(),
        });
        let mut panel = DiagramPanel::default();
        panel.set_er("captured-source".into(), objects);
        assert_eq!(panel.classes.len(), 2);
        assert_eq!(panel.workspace.relations.len(), 1);
        assert!(
            matches!(panel.navigation_event(1),DiagramEvent::DatabaseObject(source,object)if source=="captured-source"&&serde_json::from_str::<rustpy_db::workspace::Object>(&object).unwrap().name=="orders")
        );
    }
}

//! Kubernetes uses the existing transfer worker and Terminal process owners.
use super::*;
use crate::a11y::AccessibleCombo;
use crate::a11y::DialogWindow;
use rustpy_deploy::kubernetes::{Resource, Target, KINDS};
pub(super) struct State {
    root: PathBuf,
    target: Target,
    browse_target: Option<Target>,
    kind: String,
    all: bool,
    rows: Vec<Resource>,
    selected: Option<Resource>,
    contexts: Vec<String>,
    container: String,
    previous: bool,
    local: u16,
    remote: u16,
    filter: String,
    output: String,
    expected: Option<u64>,
    operation: String,
    chart: String,
    release: String,
    values: String,
    revision: u32,
    directory: String,
    manifest: String,
    confirmation: Option<(String, rustpy_deploy::CommandSpec)>,
}
impl State {
    fn resource_target(&self) -> Target {
        self.browse_target
            .clone()
            .unwrap_or_else(|| self.target.clone())
    }
    fn new(root: &Path) -> Self {
        Self {
            root: root.into(),
            target: Target::load(root),
            browse_target: None,
            kind: "pods".into(),
            all: false,
            rows: Vec::new(),
            selected: None,
            contexts: Vec::new(),
            container: String::new(),
            previous: false,
            local: 8080,
            remote: 80,
            filter: String::new(),
            output: String::new(),
            expected: None,
            operation: String::new(),
            chart: String::new(),
            release: String::new(),
            values: String::new(),
            revision: 1,
            directory: root.to_string_lossy().into_owned(),
            manifest: String::new(),
            confirmation: None,
        }
    }
}
impl DeployUi {
    pub fn open_kubernetes(&mut self, root: &Path) {
        self.capture_root(root);
        self.kubernetes = Some(State::new(root));
    }
    pub(super) fn kubernetes_result(&mut self, id: u64, operation: String, output: String) {
        if let Some(state) = &mut self.kubernetes {
            if state.expected != Some(id) {
                return;
            }
            state.expected = None;
            if operation == "Browse resources" {
                match rustpy_deploy::kubernetes::resources(&output) {
                    Ok(rows) => state.rows = rows,
                    Err(error) => state.output = error,
                }
            }
            if operation == "Contexts" {
                state.contexts = output
                    .lines()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned)
                    .collect();
            }
            state.operation = operation;
            state.output = output;
        }
    }
    pub(super) fn kubernetes_window(&mut self, ctx: &egui::Context, events: &mut Vec<DeployEvent>) {
        let Some(mut state) = self.kubernetes.take() else {
            return;
        };
        let mut open = true;
        let mut command: Option<(String, anyhow::Result<rustpy_deploy::CommandSpec>, bool)> = None;
        let mut terminal = None;
        let mut export = false;
        egui::Window::new("Kubernetes — Cluster / Helm / Kustomize").open(&mut open).default_size([960.0,760.0]).show_modeless(ctx,|ui|{egui::ScrollArea::vertical().show(ui,|ui|{
            ui.label("Installed kubectl/Helm tools use your kubeconfig credentials and credential plugins. Commands capture this context and namespace; changing this form does not redirect an active operation.");
            for(label,value)in[("kubectl executable",&mut state.target.kubectl),("Helm executable",&mut state.target.helm),("Kustomize executable (empty = kubectl kustomize)",&mut state.target.kustomize),("Kubeconfig (empty = standard KUBECONFIG discovery)",&mut state.target.kubeconfig),("Context",&mut state.target.context),("Namespace",&mut state.target.namespace)]{ui.horizontal(|ui|{ui.label(crate::i18n::tr_static(label));ui.text_edit_singleline(value);});}
            ui.horizontal(|ui|{if ui.button("Save target").clicked(){if let Err(error)=state.target.save(&state.root){state.output=error.to_string();}}
if ui.button("Load contexts").clicked(){command=Some(("Contexts".into(),state.target.contexts(),false));}egui::ComboBox::from_id_salt("kubernetes-contexts").selected_text("Choose context").show_ui(ui,|ui|{for context in &state.contexts{ui.selectable_value(&mut state.target.context,context.clone(),context);}}).combo_named(crate::i18n::tr_static("Context"));});
            ui.separator();ui.horizontal(|ui|{egui::ComboBox::from_id_salt("kubernetes-kind").selected_text(&state.kind).show_ui(ui,|ui|{for kind in KINDS{if ui.selectable_value(&mut state.kind,(*kind).into(),*kind).changed(){state.selected=None;state.rows.clear();state.expected=None;state.browse_target=None;}}}).combo_named(crate::i18n::tr_static("Resource kind"));ui.checkbox(&mut state.all,"All namespaces");if ui.button("Refresh resources").clicked(){command=Some(("Browse resources".into(),state.target.browse(&state.kind,state.all),false));}ui.label("Filter");ui.text_edit_singleline(&mut state.filter);});
            egui::ScrollArea::vertical().id_salt("kubernetes-resources").max_height(210.0).show(ui,|ui|{for row in &state.rows{if !format!("{} {} {}",row.namespace,row.name,row.phase).to_lowercase().contains(&state.filter.to_lowercase()){continue;}
if ui.selectable_label(state.selected.as_ref().is_some_and(|selected|selected.name==row.name&&selected.namespace==row.namespace),format!("{} / {}   {}",row.namespace,row.name,row.phase)).clicked(){state.selected=Some(row.clone());state.container=row.containers.first().cloned().unwrap_or_default();}}});
            if let Some(resource)=&state.selected{let mut captured=state.resource_target();if !resource.namespace.is_empty(){captured.namespace=resource.namespace.clone();}ui.label(format!("Selected {} / {}",resource.namespace,resource.name));ui.horizontal(|ui|{for(action,label)in[("describe","Describe"),("yaml","Resource YAML"),("delete","Delete resource…")]{if ui.button(crate::i18n::tr_static(label)).clicked(){command=Some((label.into(),captured.resource(action,&state.kind,&resource.name),action=="delete"));}}});if state.kind=="pods"{ui.horizontal(|ui|{ui.label("Container");ui.text_edit_singleline(&mut state.container);ui.checkbox(&mut state.previous,"Previous container");if ui.button("Logs").clicked(){command=Some(("Pod logs".into(),captured.logs(&resource.name,&state.container,state.previous),false));}
if ui.button("Follow logs in console").clicked(){terminal=Some(("Kubernetes logs".into(),captured.logs(&resource.name,&state.container,state.previous).map(|mut spec|{for argument in &mut spec.args{if argument=="--request-timeout=30s"{*argument="--request-timeout=0".into();}}spec.arg("--follow=true")})));}});}ui.horizontal(|ui|{ui.label("Loopback port");ui.add(egui::DragValue::new(&mut state.local).range(1..=65535));ui.label("Remote port");ui.add(egui::DragValue::new(&mut state.remote).range(1..=65535));if ui.button("Start port-forward console").clicked(){terminal=Some((format!("Port-forward {}:{}",resource.name,state.local),captured.port_forward(&state.kind,&resource.name,state.local,state.remote)));}});}
            ui.collapsing("Helm releases and charts",|ui|{for(label,value)in[("Release",&mut state.release),("Chart path / repository/chart",&mut state.chart),("Values file",&mut state.values)]{ui.horizontal(|ui|{ui.label(crate::i18n::tr_static(label));ui.text_edit_singleline(value);});}ui.horizontal_wrapped(|ui|{if ui.button("List releases").clicked(){command=Some(("Helm releases".into(),state.target.helm(["list","-o=json"]),false));}for(operation,label,mutates)in[("render","Render chart",false),("plan","Plan install / upgrade",false),("install","Install / upgrade…",true),("history","Release history",false),("rollback","Rollback…",true),("uninstall","Uninstall…",true)]{if ui.button(crate::i18n::tr_static(label)).clicked(){command=Some((label.into(),state.target.helm_release(&state.release,&state.chart,&state.values,operation,state.revision),mutates));}}ui.label("Revision");ui.add(egui::DragValue::new(&mut state.revision).range(1..=u32::MAX));});});
            ui.collapsing("Kustomize and manifests",|ui|{ui.horizontal(|ui|{ui.label("Kustomization directory");ui.text_edit_singleline(&mut state.directory);});ui.horizontal(|ui|{if ui.button("Render kustomization").clicked(){command=Some(("Kustomize render".into(),state.target.kustomize_plan(Path::new(&state.directory)),false));}
if ui.button("Server dry run").clicked(){command=Some(("Kustomize plan".into(),state.target.kustomize_apply(Path::new(&state.directory),true),false));}
if ui.button("Apply kustomization…").clicked(){command=Some(("Apply kustomization".into(),state.target.kustomize_apply(Path::new(&state.directory),false),true));}});ui.horizontal(|ui|{ui.label("Manifest YAML file");ui.text_edit_singleline(&mut state.manifest);});ui.horizontal(|ui|{if ui.button("Validate manifest on server").clicked(){command=Some(("Manifest plan".into(),state.target.manifest_apply(Path::new(&state.manifest),true),false));}
if ui.button("Apply manifest…").clicked(){command=Some(("Apply manifest".into(),state.target.manifest_apply(Path::new(&state.manifest),false),true));}
if ui.button("Open YAML editor").clicked()&&!state.manifest.is_empty(){events.push(DeployEvent::OpenFile(PathBuf::from(&state.manifest)));}});ui.label("The YAML editor supplies Kubernetes schema completion/inspections through the existing data-file provider. Helm template language support remains supplied by its optional editor plugin.");});
            if let Some((label,spec))=state.confirmation.clone(){ui.separator();ui.strong(format!("Confirm captured command: {label}"));ui.monospace(spec.display());ui.horizontal(|ui|{if ui.button("Run this captured command").clicked(){command=Some((label,Ok(spec),false));state.confirmation=None;}
if ui.button("Cancel operation").clicked(){state.confirmation=None;}});}
            if let Some(id)=state.expected{ui.horizontal(|ui|{crate::a11y::spinner(ui);ui.label(&state.operation);if ui.button("Cancel command").clicked(){self.stop_transfer(id);}});}
            ui.separator();ui.horizontal(|ui|{ui.strong(&state.operation);if ui.button("Copy output").clicked(){ui.ctx().copy_text(state.output.clone());}
if ui.button("Open output as YAML").clicked(){export=true;}});ui.add(egui::TextEdit::multiline(&mut state.output).font(egui::TextStyle::Monospace).desired_rows(16).desired_width(f32::INFINITY));
        });});
        if export {
            let path = state.root.join(".rustpy/kubernetes/rendered.yaml");
            let result = (|| -> std::io::Result<()> {
                std::fs::create_dir_all(path.parent().unwrap())?;
                std::fs::write(&path, &state.output)
            })();
            match result {
                Ok(()) => events.push(DeployEvent::OpenFile(path)),
                Err(error) => state.output = error.to_string(),
            }
        }
        if let Some((label, result, confirm)) = command {
            match result {
                Err(error) => state.output = error.to_string(),
                Ok(spec) if confirm => state.confirmation = Some((label, spec)),
                Ok(spec) => {
                    if label == "Browse resources" {
                        state.browse_target = Some(state.target.clone());
                        state.selected = None;
                    }
                    state.operation = label.clone();
                    self.spawn(label.clone(), move || {
                        let id = rustpy_deploy::transfer::current()
                            .owner
                            .map(|owner| owner.id)
                            .unwrap_or(0);
                        let output = rustpy_deploy::exec::run_checked(
                            &spec,
                            std::time::Duration::from_secs(180),
                            |_| None,
                        )
                        .map(|out| {
                            rustpy_deploy::kubernetes::redact(&String::from_utf8_lossy(&out.stdout))
                        })
                        .unwrap_or_else(|error| format!("Command failed: {error}"));
                        JobResult::KubernetesOutput(id, label, output)
                    });
                    state.expected = self.jobs.last().map(|job| job.owner.id);
                }
            }
        }
        if let Some((label, result)) = terminal {
            match result {
                Ok(spec) => events.push(DeployEvent::Terminal(
                    label,
                    spec.program,
                    spec.args,
                    state.root.clone(),
                )),
                Err(error) => state.output = error.to_string(),
            }
        }
        if open {
            self.kubernetes = Some(state);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn resource_actions_keep_the_browsed_cluster_after_form_changes() {
        let mut state = State::new(Path::new("/nonexistent/rustpy-kubernetes-fixture"));
        state.target.context = "production".into();
        state.browse_target = Some(state.target.clone());
        state.target.context = "development".into();
        let command = state
            .resource_target()
            .resource("describe", "pods", "api-abc")
            .unwrap();
        assert!(command.args.contains(&"--context=production".into()));
        assert!(!command.args.contains(&"--context=development".into()));
    }
    #[test]
    fn obsolete_worker_results_cannot_replace_a_new_selection() {
        let mut ui = DeployUi::default();
        let mut state = State::new(Path::new("/nonexistent/rustpy-kubernetes-fixture"));
        state.expected = Some(9);
        state.output = "new selection".into();
        ui.kubernetes = Some(state);
        ui.kubernetes_result(8, "Browse resources".into(), "obsolete".into());
        assert_eq!(ui.kubernetes.as_ref().unwrap().output, "new selection");
        ui.kubernetes_result(9, "Contexts".into(), "development\nproduction\n".into());
        assert_eq!(
            ui.kubernetes.as_ref().unwrap().contexts,
            vec!["development", "production"]
        );
    }
}

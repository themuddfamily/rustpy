//! Mercurial, SVN and Perforce tool window. Requests carry their own immutable root.
use crate::a11y::AccessibleCombo;
use crate::a11y::DialogWindow;
use crate::theme::UiTheme;
use egui::Vec2;
use rustpy_settings::other_vcs::RootSettings;
use rustpy_vcs::{
    other_vcs::{results::Data, Arguments, Credentials, Format, Operation},
    provider::Provider,
};
use std::{collections::BTreeSet, path::PathBuf};
#[derive(Clone)]
pub struct Request {
    pub root: PathBuf,
    pub provider: Provider,
    pub operation: Operation,
    pub args: Arguments,
    pub settings: RootSettings,
    pub credentials: Credentials,
}
pub enum Event {
    Run(Box<Request>),
    Open(PathBuf, Option<usize>),
    Mappings,
    SaveSettings(RootSettings),
    Export(String),
}
pub struct ResultView {
    pub request: Request,
    pub data: Data,
    pub format: Format,
    pub success: bool,
    pub error: String,
    pub truncated: bool,
}
#[derive(Default)]
pub struct OtherVcsPanel {
    pub selected: Option<(PathBuf, Provider)>,
    pub settings: RootSettings,
    pub args: Arguments,
    pub operation: Option<Operation>,
    pub result: Option<ResultView>,
    pub graph: Data,
    pub console: String,
    pub busy: Option<String>,
    pub message: String,
    target: String,
    password: String,
    remember: bool,
    checked: BTreeSet<PathBuf>,
    pending: Option<Request>,
    query: String,
    pub queued_time_lapse: bool,
    selected_version: Option<u32>,
    graph_paths: Vec<PathBuf>,
    graph_root: Option<PathBuf>,
}
impl OtherVcsPanel {
    pub fn select(&mut self, root: PathBuf, provider: Provider, settings: RootSettings) {
        if self.selected.as_ref() != Some(&(root.clone(), provider)) {
            self.selected = Some((root, provider));
            self.settings = settings;
            self.args = Arguments::default();
            self.operation = Some(Operation::Status);
            self.password.clear();
            self.checked.clear();
            self.result = None;
            self.graph = Data::default();
            self.target.clear();
            self.selected_version = None;
        }
    }
    pub fn set_target(&mut self, path: PathBuf) {
        self.target = path.to_string_lossy().into_owned();
    }
    pub fn graph_matches(&self, root: &std::path::Path, paths: &[PathBuf]) -> bool {
        self.graph_root.as_deref() == Some(root)
            && self.graph_paths == paths
            && !self.graph.graph.is_empty()
    }
    pub fn request(&mut self, operation: Operation) -> Option<Request> {
        let (root, provider) = self.selected.clone()?;
        let mut args = self.args.clone();
        args.paths = if !self.checked.is_empty() {
            self.checked.iter().cloned().collect()
        } else if !self.target.trim().is_empty() {
            vec![PathBuf::from(self.target.trim())]
        } else {
            Vec::new()
        };
        let request = Request {
            root,
            provider,
            operation,
            args,
            settings: self.settings.clone(),
            credentials: Credentials {
                username: self.settings.username.clone(),
                password: self.password.clone(),
            },
        };
        if !self.remember {
            self.password.clear();
        }
        Some(request)
    }
    pub fn append_console(&mut self, text: &str) {
        self.console.push_str(text);
        if self.console.len() > 512 * 1024 {
            let mut start = self.console.len() - 384 * 1024;
            while !self.console.is_char_boundary(start) {
                start += 1;
            }
            self.console.drain(..start);
        }
    }
    pub fn accept(&mut self, view: ResultView) {
        if self.selected.as_ref() != Some(&(view.request.root.clone(), view.request.provider)) {
            self.message = format!(
                "{} finished for {}",
                view.request.operation.label(),
                view.request.root.display()
            );
            return;
        }
        if view.request.operation == Operation::RevisionGraph && view.success {
            self.graph = view.data.clone();
            self.graph_paths = view.request.args.paths.clone();
            self.graph_root = Some(view.request.root.clone());
        }
        if view.request.operation == Operation::JobDetails && view.success {
            self.args.value = view.data.raw.clone();
        }
        if view.request.operation == Operation::TimeLapse {
            self.selected_version = view
                .request
                .args
                .revision
                .trim_start_matches('#')
                .parse()
                .ok();
        }
        self.result = Some(view);
    }
    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        theme: &UiTheme,
        roots: &[(PathBuf, Provider)],
        preferences: &rustpy_settings::other_vcs::OtherVcsSettings,
    ) -> Vec<Event> {
        let mut events = Vec::new();
        ui.horizontal_wrapped(|ui| {
            ui.heading("Version Control");
            for (root, provider) in roots {
                let label = format!(
                    "{} · {}",
                    if *provider == Provider::Subversion {
                        "SVN"
                    } else {
                        provider.label()
                    },
                    root.file_name()
                        .unwrap_or(root.as_os_str())
                        .to_string_lossy()
                );
                if ui
                    .selectable_label(
                        self.selected.as_ref() == Some(&(root.clone(), *provider)),
                        label,
                    )
                    .on_hover_text(root.display().to_string())
                    .clicked()
                {
                    self.select(root.clone(), *provider, preferences.root(root));
                }
            }
            if ui
                .button("Directory mappings / Enable Integration…")
                .clicked()
            {
                events.push(Event::Mappings);
            }
        });
        if self.selected.is_none() {
            if let Some((root, provider)) = roots.first() {
                self.select(root.clone(), *provider, preferences.root(root));
            }
        }
        let Some((root, provider)) = self.selected.clone() else {
            ui.label(
                "Add a Mercurial, SVN or Perforce directory mapping to use its bundled adapter.",
            );
            return events;
        };
        ui.label(format!(
            "{} tool window — {}",
            if provider == Provider::Subversion {
                "SVN"
            } else {
                provider.label()
            },
            root.display()
        ));
        if let Some(busy) = &self.busy {
            ui.colored_label(theme.warning, busy);
        }
        if !self.message.is_empty() {
            ui.label(&self.message);
        }
        ui.collapsing("Connection and provider preferences",|ui| {
            ui.horizontal(|ui|{ui.label(if provider==Provider::Perforce{"Server"}else{"Remote / repository URL"});ui.text_edit_singleline(&mut self.settings.endpoint);});
            ui.horizontal(|ui|{ui.label("Username");ui.text_edit_singleline(&mut self.settings.username);});
            if matches!(provider,Provider::Perforce|Provider::Tfs) {ui.horizontal(|ui|{ui.label("Workspace");ui.text_edit_singleline(&mut self.settings.client);});}
            if provider==Provider::Tfs {self.password.clear();self.remember=false;ui.label("Uses the installed Visual Studio tf.exe credential manager. Sign in there before running /noprompt commands; no password is placed in argv.");}
            else {ui.horizontal(|ui|{ui.label("Password / HTTPS token (session only)");ui.add(egui::TextEdit::singleline(&mut self.password).password(true));});ui.checkbox(&mut self.remember,"Keep the password in memory while this root remains selected");}
            if provider==Provider::Subversion {ui.checkbox(&mut self.settings.allow_auth_cache,"Allow SVN to use its credential cache");ui.horizontal(|ui|{ui.label("Trusted CA certificate file (optional)");ui.text_edit_singleline(&mut self.settings.svn_ca_file);});}
            if provider==Provider::Mercurial {ui.checkbox(&mut self.settings.hg_update_after_pull,"Update working copy after pull");ui.label("SSH keys and passphrases use the IDE ask-pass dialog; HTTPS passwords require an explicit HTTPS remote above.");}
            ui.horizontal(|ui|{ui.label("History page size");ui.add(egui::DragValue::new(&mut self.settings.history_limit).range(1..=10000));ui.label("Timeout (seconds)");ui.add(egui::DragValue::new(&mut self.settings.timeout_seconds).range(10..=3600));});
            if ui.button("Save provider preferences").clicked(){events.push(Event::SaveSettings(self.settings.clone()));}
        });
        let mut run = None;
        ui.horizontal_wrapped(|ui| {
            for op in [
                Operation::Status,
                Operation::Log,
                Operation::Diff,
                if provider == Provider::Perforce {
                    Operation::Submit
                } else {
                    Operation::Commit
                },
                if provider == Provider::Perforce {
                    Operation::Sync
                } else {
                    Operation::Update
                },
            ] {
                if Operation::available(provider).contains(&op)
                    && ui
                        .add_enabled(self.busy.is_none(), egui::Button::new(op.label()))
                        .clicked()
                {
                    self.operation = Some(op);
                    if matches!(op, Operation::Status | Operation::Log | Operation::Diff) {
                        run = Some(op);
                    }
                }
            }
            egui::ComboBox::from_id_salt("other_vcs_operation")
                .selected_text(self.operation.unwrap_or(Operation::Status).label())
                .show_ui(ui, |ui| {
                    for op in Operation::available(provider) {
                        ui.selectable_value(&mut self.operation, Some(*op), op.label());
                    }
                })
                .combo_named(crate::i18n::tr_static("Operation"));
        });
        let op = self.operation.unwrap_or(Operation::Status);
        ui.horizontal(|ui| {
            ui.label("File / directory / depot path");
            ui.add(
                egui::TextEdit::singleline(&mut self.target)
                    .hint_text("Use the current root when the operation supports it")
                    .desired_width(450.0),
            );
            if !self.checked.is_empty() {
                ui.label(format!("{} selected path(s)", self.checked.len()));
                if ui.small_button("Clear selection").clicked() {
                    self.checked.clear();
                }
            }
        });
        self.fields(ui, provider, op);
        ui.horizontal(|ui| {
            if ui
                .add_enabled(self.busy.is_none(), egui::Button::new(op.label()))
                .clicked()
            {
                run = Some(op);
            }
            if provider == Provider::Perforce
                && matches!(op, Operation::TimeLapse | Operation::PrintRevision)
                && !self.graph.graph.is_empty()
            {
                let mut versions = self
                    .graph
                    .graph
                    .iter()
                    .map(|node| node.revision)
                    .collect::<Vec<_>>();
                versions.sort();
                versions.dedup();
                let current = self
                    .selected_version
                    .unwrap_or_else(|| versions.last().copied().unwrap_or(1));
                egui::ComboBox::from_id_salt("p4_time_lapse_revision")
                    .selected_text(format!("Revision #{current}"))
                    .show_ui(ui, |ui| {
                        for revision in versions {
                            if ui
                                .selectable_label(current == revision, format!("#{revision}"))
                                .clicked()
                            {
                                self.selected_version = Some(revision);
                                self.args.revision = revision.to_string();
                                run = Some(op);
                            }
                        }
                    })
                    .combo_named(crate::i18n::tr_static("Revision"));
            }
        });
        if let Some(op) = run {
            if let Some(request) = self.request(op) {
                if op.destructive() || (op == Operation::Update && request.args.force) {
                    self.pending = Some(request);
                } else {
                    events.push(Event::Run(Box::new(request)));
                }
            }
        }
        if let Some(request) = self.pending.clone() {
            egui::Window::new(format!("{} — confirm", request.operation.label()))
                .id(egui::Id::new("provider_destructive_action"))
                .collapsible(false)
                .show_modal(ui.ctx(), |ui| {
                    ui.label(format!(
                        "{} at {}",
                        request.operation.label(),
                        request.root.display()
                    ));
                    ui.label(format!("{} selected path(s)", request.args.paths.len()));
                    ui.horizontal(|ui| {
                        if ui.button("Continue").clicked() {
                            events.push(Event::Run(Box::new(request.clone())));
                            self.pending = None;
                        }
                        if ui.button("Cancel").clicked() {
                            self.pending = None;
                        }
                    });
                });
        }
        ui.separator();
        ui.horizontal(|ui| {
            ui.label("Filter results");
            ui.text_edit_singleline(&mut self.query);
            if let Some(result) = &self.result {
                if ui.button("Export result…").clicked() {
                    events.push(Event::Export(result.data.raw.clone()));
                }
            }
        });
        self.results_ui(ui, theme, &mut events);
        ui.collapsing("Provider console", |ui| {
            egui::ScrollArea::both()
                .id_salt("provider_console")
                .max_height(250.0)
                .stick_to_bottom(true)
                .show(ui, |ui| {
                    ui.add(
                        egui::Label::new(egui::RichText::new(&self.console).monospace())
                            .selectable(true),
                    );
                });
        });
        events
    }
    fn fields(&mut self, ui: &mut egui::Ui, provider: Provider, op: Operation) {
        use Operation::*;
        let field = |ui: &mut egui::Ui, label: &str, value: &mut String| {
            ui.horizontal(|ui| {
                ui.label(label);
                ui.add(egui::TextEdit::singleline(value).desired_width(450.0));
            });
        };
        if matches!(
            op,
            Commit | QueueNew | CreateChangelist | CopyTag | Import | Lock
        ) || (op == Submit && self.args.change.is_empty())
        {
            ui.label("Message / description");
            ui.add(
                egui::TextEdit::multiline(&mut self.args.message)
                    .desired_rows(3)
                    .desired_width(f32::INFINITY),
            );
        }
        if matches!(
            op,
            Update | Merge | Graft | Rebase | Annotate | Diff | Sync | TimeLapse | PrintRevision
        ) {
            field(
                ui,
                if provider == Provider::Subversion {
                    "Revision or revision range"
                } else {
                    "Revision / branch / destination"
                },
                &mut self.args.revision,
            );
        }
        if matches!(
            op,
            CreateBranch
                | Bookmark
                | QueueNew
                | QueuePush
                | SetProperty
                | Jobs
                | JobDetails
                | FixJob
                | UnfixJob
        ) || (matches!(op, Shelve | Unshelve)
            && matches!(provider, Provider::Mercurial | Provider::Tfs))
            || (op == AssignChangelist && provider == Provider::Subversion)
        {
            field(
                ui,
                if op == Jobs {
                    "Job filter expression"
                } else if op == SetProperty {
                    "Property name"
                } else {
                    "Name"
                },
                &mut self.args.name,
            );
        }
        if (provider == Provider::Subversion
            && matches!(
                op,
                Checkout | Branches | CopyTag | Merge | Integrate | Relocate
            ))
            || (provider == Provider::Perforce && matches!(op, Unshelve | Integrate))
            || (provider == Provider::Tfs && matches!(op, Initialize | MapWorkspace))
        {
            field(
                ui,
                if op == Unshelve {
                    "Source changelist"
                } else {
                    "Source URL / depot path"
                },
                &mut self.args.source,
            );
        }
        if (provider == Provider::Subversion
            && matches!(op, Checkout | Import | CopyTag | Relocate))
            || (provider == Provider::Perforce && op == Integrate)
        {
            field(
                ui,
                if op == Checkout {
                    "Checkout directory"
                } else {
                    "Destination URL / depot path"
                },
                &mut self.args.destination,
            );
        }
        if provider == Provider::Perforce
            && matches!(
                op,
                Submit | Shelve | Unshelve | Describe | AssignChangelist | FixJob | UnfixJob
            )
        {
            field(
                ui,
                if op == Submit {
                    "Numbered changelist (empty submits default)"
                } else {
                    "Target changelist"
                },
                &mut self.args.change,
            );
        }
        if op == Trust {
            field(ui, "Verified SSL server fingerprint", &mut self.args.value);
            ui.label("Verify the fingerprint independently before installing it in the Perforce trust store.");
        }
        if matches!(op, SetProperty | Ignore | SaveJob) {
            ui.label(if op == SaveJob {
                "Perforce job form"
            } else {
                "Property value (one ignore pattern per line)"
            });
            ui.add(
                egui::TextEdit::multiline(&mut self.args.value)
                    .desired_rows(5)
                    .desired_width(f32::INFINITY),
            );
        }
        if matches!(op, Update | Sync | Unlock | Trust) {
            ui.checkbox(
                &mut self.args.force,
                "Force / discard conflicting local state",
            );
        }
        if provider == Provider::Subversion && matches!(op, Merge | Integrate) {
            ui.checkbox(
                &mut self.args.dry_run,
                "Preview merge without changing the working copy",
            );
        }
    }
    fn results_ui(&mut self, ui: &mut egui::Ui, theme: &UiTheme, events: &mut Vec<Event>) {
        let Some(result) = &self.result else { return };
        ui.label(format!(
            "{} · {}",
            result.request.operation.label(),
            result.request.root.display()
        ));
        if !result.request.args.paths.is_empty() {
            ui.label(
                result
                    .request
                    .args
                    .paths
                    .iter()
                    .map(|path| path.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", "),
            );
        }
        if !result.success {
            ui.colored_label(theme.error, &result.error);
        } else if !result.error.trim().is_empty() {
            ui.collapsing("Provider messages", |ui| {
                ui.label(&result.error);
            });
        }
        if result.truncated {
            ui.colored_label(theme.warning,"Provider output reached the display limit. Narrow the query or select fewer paths.");
        }
        let query = self.query.to_lowercase();
        if result.format == Format::RevisionGraph {
            egui::ScrollArea::both()
                .id_salt("provider_revision_graph")
                .max_height(340.0)
                .show(ui, |ui| {
                    for node in &result.data.graph {
                        ui.horizontal(|ui| {
                            let (rect, _) =
                                ui.allocate_exact_size(Vec2::new(18.0, 30.0), egui::Sense::hover());
                            let center = rect.center();
                            ui.painter().line_segment(
                                [
                                    egui::pos2(center.x, rect.top()),
                                    egui::pos2(center.x, rect.bottom()),
                                ],
                                egui::Stroke::new(1.0, theme.text_dim),
                            );
                            ui.painter().circle_filled(center, 4.0, theme.accent);
                            if ui
                                .link(format!(
                                    "{}#{} · change {} · {}",
                                    node.file, node.revision, node.change, node.user
                                ))
                                .clicked()
                            {
                                self.target = node.file.clone();
                                self.args.revision = node.revision.to_string();
                                self.operation = Some(Operation::TimeLapse);
                            }
                            ui.label(&node.description);
                        });
                        for edge in result.data.edges.iter().filter(|edge| edge.from == node.id) {
                            ui.indent(&edge.to, |ui| {
                                if ui
                                    .link(format!("{} → {}", edge.relation, edge.to))
                                    .clicked()
                                {
                                    if let Some((file, rev)) = edge.to.rsplit_once('#') {
                                        self.target = file.into();
                                        self.args.revision = rev.into();
                                        self.operation = Some(Operation::TimeLapse);
                                    }
                                }
                            });
                        }
                    }
                });
            return;
        }
        if !result.data.rows.is_empty() {
            egui::ScrollArea::both()
                .id_salt("provider_results")
                .max_height(380.0)
                .show(ui, |ui| {
                    for row in &result.data.rows {
                        if !query.is_empty()
                            && !format!("{} {}", row.label, row.detail)
                                .to_lowercase()
                                .contains(&query)
                        {
                            continue;
                        }
                        ui.horizontal(|ui| {
                            if let Some(path) = &row.path {
                                let mut checked = self.checked.contains(path);
                                if ui.checkbox(&mut checked, "").changed() {
                                    if checked {
                                        self.checked.insert(path.clone());
                                    } else {
                                        self.checked.remove(path);
                                    }
                                }
                            }
                            let response = ui.selectable_label(false, &row.label);
                            if response.clicked() {
                                if let Some(revision) = &row.revision {
                                    self.args.revision = revision.clone();
                                }
                                if let Some(path) = &row.path {
                                    self.target = path.to_string_lossy().into_owned();
                                }
                                if let Some(value) = row.fields.get("value") {
                                    self.args.value = value.clone();
                                }
                                if let Some(name) = row.fields.get("name") {
                                    self.args.name = name.clone();
                                }
                                if let Some(change) = row.fields.get("change") {
                                    self.args.change = change.clone();
                                }
                            }
                            if response.double_clicked() {
                                if let Some(path) = &row.path {
                                    events.push(Event::Open(path.clone(), None));
                                } else if matches!(
                                    result.format,
                                    Format::Annotation | Format::TimeLapse
                                ) {
                                    if let Some(path) =
                                        result.request.args.paths.first().filter(|path| {
                                            !path.to_string_lossy().starts_with("//")
                                        })
                                    {
                                        events.push(Event::Open(
                                            path.clone(),
                                            row.id
                                                .parse::<usize>()
                                                .ok()
                                                .map(|line| line.saturating_sub(1)),
                                        ));
                                    }
                                }
                            }
                            if !row.detail.is_empty() {
                                ui.label(
                                    egui::RichText::new(&row.detail)
                                        .small()
                                        .color(theme.text_dim),
                                );
                            }
                        });
                        if !row.fields.is_empty() {
                            ui.collapsing(format!("Details · {}", row.id), |ui| {
                                for (name, value) in &row.fields {
                                    ui.label(format!("{name}: {value}"));
                                }
                            });
                        }
                    }
                });
        } else {
            egui::ScrollArea::both()
                .id_salt("provider_text_result")
                .max_height(420.0)
                .show(ui, |ui| {
                    ui.add(
                        egui::Label::new(egui::RichText::new(&result.data.raw).monospace())
                            .selectable(true),
                    );
                });
        }
    }
}

//! Shared branch popup/tool-window tree. Commands carry explicit selections.
use crate::a11y::DialogWindow;
use crate::theme::UiTheme;
use rustpy_settings::branches::BranchPreferences;
use rustpy_vcs::{
    branches::{BranchComparison, BranchRecord, LocalAction, MergeMode},
    GitRepo,
};
use std::{collections::BTreeMap, path::PathBuf};
#[derive(Clone, Debug)]
pub enum Command {
    Checkout(String),
    CheckoutRemote(String),
    SmartCheckout(String),
    Rebase(String),
    Pull(String, bool),
    UpdateBranch(String),
    Local(LocalAction),
    Compare(String),
    WorkingDiff(String),
    CommitDiff(String),
    Network { label: String, args: Vec<String> },
    Fetch,
    Remotes,
    SelectRoot(PathBuf),
    ShowCommit(String),
    ShowWindow,
    CreatePullRequest(String),
}
#[derive(Clone, Debug)]
pub struct Event {
    pub root: PathBuf,
    pub command: Command,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FormKind {
    New,
    Rename,
    Merge,
    Track,
    Delete,
    Tag,
    Revision,
    CheckoutChoice,
    Push,
    DeleteTag,
}
struct Form {
    kind: FormKind,
    target: String,
    name: String,
    upstream: String,
    message: String,
    checkout: bool,
    overwrite: bool,
    force: bool,
    mode: MergeMode,
}
impl Form {
    fn new(kind: FormKind, target: String) -> Self {
        Self {
            kind,
            target,
            name: String::new(),
            upstream: String::new(),
            message: String::new(),
            checkout: true,
            overwrite: false,
            force: false,
            mode: MergeMode::default(),
        }
    }
}
#[derive(Default)]
pub struct Workspace {
    pub preferences: BranchPreferences,
    pub menu_available: bool,
    pub preferences_dirty: bool,
    pub roots: Vec<PathBuf>,
    pub tags_open: bool,
    pub root: PathBuf,
    records: Vec<BranchRecord>,
    other_records: BTreeMap<PathBuf, Vec<BranchRecord>>,
    refresh: f64,
    filter: String,
    selected: Option<String>,
    form: Option<Form>,
    pub blocked: Option<String>,
    pub restore: Option<(String, String)>,
    pub comparison: Option<BranchComparison>,
    pub error: Option<String>,
    pub pending: Vec<BranchJob>,
}
impl Workspace {
    pub fn invalidate(&mut self) {
        self.refresh = -100.0;
    }
    pub fn set_preferences(&mut self, prefs: &BranchPreferences) {
        if !self.preferences_dirty {
            self.preferences = prefs.clone();
        }
    }
    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        theme: &UiTheme,
        repo: &GitRepo,
        now: f64,
    ) -> Vec<Event> {
        if !self.menu_available {
            ui.disable();
        }
        let root = repo.root().to_path_buf();
        let key = root.display().to_string();
        let mut commands = Vec::new();
        let mut scoped_events = Vec::new();
        if root != self.root {
            self.selected = None;
            self.form = None;
            self.comparison = None;
            self.blocked = None;
            self.restore = None;
            self.error = None;
        }
        let references_changed = !self
            .records
            .iter()
            .map(|record| &record.branch)
            .eq(repo.branches().iter());
        if root != self.root || references_changed || now - self.refresh > 2.0 {
            self.records = repo.branch_inventory();
            self.other_records = self
                .roots
                .iter()
                .filter(|candidate| candidate.as_path() != repo.root())
                .filter_map(|root| {
                    GitRepo::discover(root).map(|repo| (root.clone(), repo.branch_inventory()))
                })
                .collect();
            self.root = root.clone();
            self.refresh = now;
        }
        if self.menu_available
            && !self.pending.is_empty()
            && ui.button("Cancel branch operation").clicked()
        {
            self.cancel_jobs();
        }
        if let Some(operation) = repo.running_operation() {
            ui.label(format!("{operation} is running"));
            ui.disable();
        }
        let before = self.preferences.clone();
        ui.horizontal_wrapped(|ui| {
            ui.label("Search");
            ui.add(egui::TextEdit::singleline(&mut self.filter).desired_width(180.0));
            if ui.button("New Branch…").clicked() {
                self.form = Some(Form::new(
                    FormKind::New,
                    self.selected.clone().unwrap_or_else(|| "HEAD".into()),
                ));
            }
            if ui.button("Checkout tag or revision…").clicked() {
                self.form = Some(Form::new(FormKind::Revision, String::new()));
            }
            if ui.button("Fetch all / prune").clicked() {
                commands.push(Command::Fetch);
            }
            if ui.button("Remotes…").clicked() {
                commands.push(Command::Remotes);
            }
            if ui.button("Open Branches tool window").clicked() {
                commands.push(Command::ShowWindow);
            }
            if ui.button("Tags…").clicked() {
                self.tags_open = true;
            }
            if ui.button("New tag…").clicked() {
                self.form = Some(Form::new(
                    FormKind::Tag,
                    self.selected.clone().unwrap_or_else(|| "HEAD".into()),
                ));
            }
            ui.menu_button("View", |ui| {
                ui.checkbox(&mut self.preferences.group_prefix, "Group by prefix");
                ui.checkbox(
                    &mut self.preferences.group_repository,
                    "Group by repository",
                );
                ui.checkbox(&mut self.preferences.show_details, "Show details");
                ui.checkbox(
                    &mut self.preferences.ask_checkout_rebase,
                    "Ask whether to check out with rebase",
                );
            });
        });
        ui.menu_button("Default push remote", |ui| {
            for remote in repo.remotes() {
                if ui.button(&remote).clicked() {
                    commands.push(Command::Local(LocalAction::DefaultRemote(remote)));
                    ui.close();
                }
            }
        });
        if self.preferences.group_repository {
            ui.horizontal_wrapped(|ui| {
                ui.label("Repositories");
                for candidate in &self.roots {
                    if ui
                        .selectable_label(
                            candidate == &root,
                            candidate.file_name().unwrap_or_default().to_string_lossy(),
                        )
                        .on_hover_text(candidate.display().to_string())
                        .clicked()
                    {
                        commands.push(Command::SelectRoot(candidate.clone()));
                    }
                }
            });
        }
        if let Some(error) = &self.error {
            ui.colored_label(theme.error, error);
        }
        if let Some((name, oid)) = self.restore.clone() {
            ui.horizontal(|ui| {
                ui.label(format!("Deleted '{name}'."));
                if ui.link("Restore branch").clicked() {
                    commands.push(Command::Local(LocalAction::Restore { branch: name, oid }));
                    self.restore = None;
                }
            });
        }
        if let Some(target) = self.blocked.clone() {
            ui.group(|ui|{ui.colored_label(theme.warning,format!("Local changes block checkout of '{target}'."));ui.horizontal_wrapped(|ui|{
 if ui.button("Smart checkout (stash / reapply)").clicked(){commands.push(Command::SmartCheckout(target.clone()));}
 if ui.button("Shelve and checkout").on_hover_text("Preserve binary files, index and untracked files in a durable branch shelf; restore explicitly later").clicked(){commands.push(Command::Local(LocalAction::ShelfCheckout(target.clone())));}
 if ui.button("Force checkout…").clicked(){let mut form=Form::new(FormKind::CheckoutChoice,target.clone());form.force=true;self.form=Some(form);}
 if ui.button("Cancel").clicked(){self.blocked=None;}
 });});
        }
        let deleted = repo.deleted_branches();
        if !deleted.is_empty() {
            egui::CollapsingHeader::new("Recently deleted branches").show(ui, |ui| {
                for (branch, oid) in deleted {
                    ui.horizontal(|ui| {
                        ui.label(&branch);
                        if ui.link("Restore").clicked() {
                            commands.push(Command::Local(LocalAction::Restore { branch, oid }));
                        }
                    });
                }
            });
        }
        for (reference, message) in repo.branch_shelves() {
            ui.horizontal(|ui| {
                ui.label(message);
                if ui.link("Restore branch shelf").clicked() {
                    commands.push(Command::Local(LocalAction::Unshelve(reference)));
                }
            });
        }
        let filter = self.filter.to_lowercase();
        let records = self.records.clone();
        let visible: Vec<_> = records
            .iter()
            .filter(|record| {
                filter.is_empty() || record.branch.name.to_lowercase().contains(&filter)
            })
            .collect();
        egui::ScrollArea::vertical()
            .id_salt("branch-workspace-tree")
            .max_height(430.0)
            .show(ui, |ui| {
                let favorite: Vec<_> = visible
                    .iter()
                    .copied()
                    .filter(|record| self.preferences.favorite(&key, &record.branch.name))
                    .collect();
                self.section(ui, "Favorites", favorite, &key, &mut commands);
                let recent = self
                    .preferences
                    .recent
                    .get(&key)
                    .cloned()
                    .unwrap_or_default();
                let recent: Vec<_> = recent
                    .iter()
                    .filter_map(|name| {
                        visible
                            .iter()
                            .copied()
                            .find(|record| &record.branch.name == name)
                    })
                    .collect();
                self.section(ui, "Recent", recent, &key, &mut commands);
                for (remote, label) in [(false, "Local branches"), (true, "Remote branches")] {
                    let entries: Vec<_> = visible
                        .iter()
                        .copied()
                        .filter(|record| record.branch.is_remote == remote)
                        .collect();
                    egui::CollapsingHeader::new(label)
                        .default_open(true)
                        .show(ui, |ui| {
                            if self.preferences.group_prefix {
                                let mut groups: BTreeMap<String, Vec<_>> = BTreeMap::new();
                                for record in entries {
                                    groups
                                        .entry(
                                            record
                                                .branch
                                                .name
                                                .rsplit_once('/')
                                                .map(|(prefix, _)| prefix)
                                                .unwrap_or("(ungrouped)")
                                                .into(),
                                        )
                                        .or_default()
                                        .push(record);
                                }
                                for (prefix, entries) in groups {
                                    egui::CollapsingHeader::new(prefix).default_open(true).show(
                                        ui,
                                        |ui| {
                                            for record in entries {
                                                self.row(ui, record, &key, &mut commands);
                                            }
                                        },
                                    );
                                }
                            } else {
                                for record in entries {
                                    self.row(ui, record, &key, &mut commands);
                                }
                            }
                        });
                }
                egui::CollapsingHeader::new("Tags").show(ui, |ui| {
                    for (tag, oid) in repo.tags().unwrap_or_default() {
                        if !filter.is_empty() && !tag.to_lowercase().contains(&filter) {
                            continue;
                        }
                        ui.horizontal(|ui| {
                            ui.label(&tag).on_hover_text(oid);
                            if ui.small_button("Checkout").clicked() {
                                commands.push(Command::Local(LocalAction::CheckoutRevision(
                                    format!("refs/tags/{tag}"),
                                )));
                            }
                            if ui.small_button("Push…").clicked() {
                                let mut form =
                                    Form::new(FormKind::Push, format!("refs/tags/{tag}"));
                                form.upstream = repo.remotes().first().cloned().unwrap_or_default();
                                self.form = Some(form);
                            }
                            if ui.small_button("Delete…").clicked() {
                                self.form = Some(Form::new(FormKind::DeleteTag, tag));
                            }
                        });
                    }
                });
            });
        let repositories = self.other_records.clone();
        for (repository, records) in repositories {
            let repository_key = repository.display().to_string();
            let mut select = false;
            let grouped = self.preferences.group_repository;
            let mut render = |ui: &mut egui::Ui| {
                if ui.button("Select repository to manage branches").clicked() {
                    select = true;
                }
                for record in records.iter().filter(|record| {
                    filter.is_empty() || record.branch.name.to_lowercase().contains(&filter)
                }) {
                    ui.horizontal(|ui| {
                        let favorite = self
                            .preferences
                            .favorite(&repository_key, &record.branch.name);
                        if ui.small_button(if favorite { "★" } else { "☆" }).clicked() {
                            self.preferences
                                .toggle_favorite(&repository_key, &record.branch.name);
                        }
                        if ui
                            .selectable_label(
                                false,
                                format!(
                                    "{}{} ↓{} ↑{}",
                                    if record.branch.is_current { "● " } else { "" },
                                    record.branch.name,
                                    record.incoming,
                                    record.outgoing
                                ),
                            )
                            .on_hover_text(&record.summary)
                            .double_clicked()
                        {
                            select = true;
                        }
                    });
                }
            };
            if grouped {
                egui::CollapsingHeader::new(repository.display().to_string())
                    .id_salt(("branch-repository", &repository))
                    .show(ui, |ui| render(ui));
            } else {
                ui.strong(repository.file_name().unwrap_or_default().to_string_lossy());
                render(ui);
            }
            if select {
                scoped_events.push(Event {
                    root: root.clone(),
                    command: Command::SelectRoot(repository),
                });
            }
        }
        if let Some(selected) = &self.selected {
            if let Some(record) = self
                .records
                .iter()
                .find(|record| &record.branch.name == selected)
            {
                if self.preferences.show_details {
                    ui.separator();
                    ui.strong(&record.branch.name);
                    ui.label(format!(
                        "{} · {} · {}",
                        &record.oid[..7.min(record.oid.len())],
                        record.author,
                        record.summary
                    ));
                    ui.label(format!(
                        "↓ {} incoming · ↑ {} outgoing · {} not merged into HEAD",
                        record.incoming, record.outgoing, record.unmerged
                    ));
                    if let Some(up) = &record.branch.upstream {
                        ui.label(format!("Tracks {up}"));
                    } else if !record.branch.is_remote {
                        ui.label("No tracked branch");
                    }
                    if let Some(url) = repo.branch_pull_request_url(&record.branch.name) {
                        ui.hyperlink_to("Create pull request on GitHub", url);
                    }
                }
            }
        }
        if let Some(comparison) = &self.comparison {
            ui.separator();
            ui.strong(format!("{} ↔ {}", comparison.current, comparison.selected));
            ui.columns(2, |columns| {
                for (index, (label, commits, more)) in [
                    (
                        "Only in current",
                        &comparison.only_current,
                        comparison.current_more,
                    ),
                    (
                        "Only in selected",
                        &comparison.only_selected,
                        comparison.selected_more,
                    ),
                ]
                .into_iter()
                .enumerate()
                {
                    let column = &mut columns[index];
                    column.strong(label);
                    for commit in commits {
                        if column
                            .selectable_label(
                                false,
                                format!("{} {}", commit.short_id, commit.summary),
                            )
                            .clicked()
                        {
                            commands.push(Command::ShowCommit(commit.id.clone()));
                        }
                    }
                    if more {
                        column.label("Additional commits omitted (2,000 maximum)");
                    }
                }
            });
        }
        if self.tags_open {
            let mut open = true;
            egui::Window::new("Tags")
                .open(&mut open)
                .show_modeless(ui.ctx(), |ui| {
                    if ui.button("Create tag…").clicked() {
                        self.form = Some(Form::new(FormKind::Tag, "HEAD".into()));
                    }
                    for (tag, oid) in repo.tags().unwrap_or_default() {
                        ui.horizontal(|ui| {
                            ui.label(&tag).on_hover_text(oid);
                            if ui.button("Checkout").clicked() {
                                commands.push(Command::Local(LocalAction::CheckoutRevision(
                                    format!("refs/tags/{tag}"),
                                )));
                            }
                            if ui.button("Push…").clicked() {
                                self.form =
                                    Some(Form::new(FormKind::Push, format!("refs/tags/{tag}")));
                            }
                            if ui.button("Delete…").clicked() {
                                self.form = Some(Form::new(FormKind::DeleteTag, tag));
                            }
                        });
                    }
                });
            self.tags_open = open && self.form.is_none();
        }
        self.form_ui(ui, repo, &mut commands);
        if self.preferences != before {
            self.preferences_dirty = true;
        }
        scoped_events.extend(commands.into_iter().map(|command| Event {
            root: root.clone(),
            command,
        }));
        scoped_events
    }
    fn section(
        &mut self,
        ui: &mut egui::Ui,
        label: &str,
        records: Vec<&BranchRecord>,
        key: &str,
        commands: &mut Vec<Command>,
    ) {
        if records.is_empty() {
            return;
        }
        egui::CollapsingHeader::new(label)
            .default_open(true)
            .show(ui, |ui| {
                for record in records {
                    self.row(ui, record, key, commands);
                }
            });
    }
    fn row(
        &mut self,
        ui: &mut egui::Ui,
        record: &BranchRecord,
        key: &str,
        commands: &mut Vec<Command>,
    ) {
        let branch = &record.branch;
        ui.push_id((&self.root,&branch.name),|ui|{
 ui.horizontal(|ui|{let favorite=self.preferences.favorite(key,&branch.name);if ui.small_button(if favorite{"★"}else{"☆"}).on_hover_text("Toggle favorite").clicked(){self.preferences.toggle_favorite(key,&branch.name);}
 if branch.is_current{ui.label("●");}let response=ui.selectable_label(self.selected.as_deref()==Some(&branch.name),&branch.name);ui.label(format!("↓{} ↑{}",record.incoming,record.outgoing));
 if response.clicked(){self.selected=Some(branch.name.clone());}
if response.double_clicked()&&!branch.is_current{if self.preferences.ask_checkout_rebase{self.form=Some(Form::new(FormKind::CheckoutChoice,branch.name.clone()));}else{commands.push(if branch.is_remote{Command::CheckoutRemote(branch.name.clone())}else{Command::Checkout(branch.name.clone())});}}
 crate::a11y::bound_keyboard_context_menu(&response,self.menu_available,egui::Id::new((key,&branch.name,branch.is_current,&branch.upstream))).show(|ui|{
 if !branch.is_current {if ui.button("Checkout").clicked(){if self.preferences.ask_checkout_rebase{self.form=Some(Form::new(FormKind::CheckoutChoice,branch.name.clone()));}else{commands.push(if branch.is_remote{Command::CheckoutRemote(branch.name.clone())}else{Command::Checkout(branch.name.clone())});}ui.close();}
 if ui.button("Checkout and Rebase onto current").clicked(){commands.push(Command::Local(LocalAction::CheckoutRebase(branch.name.clone())));ui.close();}
 if ui.button("Rebase current onto selected").clicked(){commands.push(Command::Rebase(branch.name.clone()));ui.close();}
 if ui.button("Merge into current…").clicked(){self.form=Some(Form::new(FormKind::Merge,branch.name.clone()));ui.close();}}
 if ui.button("Create Pull Request…").clicked(){commands.push(Command::CreatePullRequest(branch.name.clone()));ui.close();}
 for (label,command) in [("Compare with current",Command::CommitDiff(branch.name.clone())),("Compare branches (two-way commits)",Command::Compare(branch.name.clone())),("Show diff with working tree",Command::WorkingDiff(branch.name.clone())),("Show diff between branches",Command::CommitDiff(branch.name.clone()))]{if ui.button(crate::i18n::tr_static(label)).clicked(){commands.push(command);ui.close();}}
 if ui.button(format!("New Branch from '{}'…",branch.name)).clicked(){self.form=Some(Form::new(FormKind::New,branch.name.clone()));ui.close();}
 if branch.is_remote{if ui.button("Track").clicked(){commands.push(Command::Local(LocalAction::TrackRemote(branch.name.clone())));ui.close();}for (label,rebase) in [("Pull into current using merge",false),("Pull into current using rebase",true)]{if ui.button(crate::i18n::tr_static(label)).clicked(){commands.push(Command::Pull(branch.name.clone(),rebase));ui.close();}}}
 else{
 if ui.button("Push…").clicked(){let mut form=Form::new(FormKind::Push,format!("refs/heads/{}",branch.name));form.name=branch.name.clone();self.form=Some(form);ui.close();}
 if ui.add_enabled(branch.upstream.is_some(),egui::Button::new("Update branch")).clicked(){commands.push(Command::UpdateBranch(branch.name.clone()));ui.close();}
 if ui.button("Edit tracked branch…").clicked(){let mut form=Form::new(FormKind::Track,branch.name.clone());form.upstream=branch.upstream.clone().unwrap_or_default();self.form=Some(form);ui.close();}
 if ui.add_enabled(branch.upstream.is_some(),egui::Button::new("Untrack")).clicked(){commands.push(Command::Local(LocalAction::Track{branch:branch.name.clone(),upstream:None}));ui.close();}
 if ui.button("Rename…").clicked(){let mut form=Form::new(FormKind::Rename,branch.name.clone());form.name=branch.name.clone();self.form=Some(form);ui.close();}
 if ui.add_enabled(!branch.is_current,egui::Button::new("Delete…")).clicked(){let mut form=Form::new(FormKind::Delete,branch.name.clone());form.message=format!("{} commit(s) are not merged into current. Deletion can be restored from the notification.",record.unmerged);self.form=Some(form);ui.close();}
 }
 });});});
    }
    fn form_ui(&mut self, ui: &mut egui::Ui, repo: &GitRepo, commands: &mut Vec<Command>) {
        let Some(form) = &mut self.form else { return };
        let mut cancel = false;
        let mut apply = false;
        ui.separator();
        ui.group(|ui|{
 ui.strong(format!("{:?}: {}",form.kind,form.target));
 match form.kind{
 FormKind::New=>{ui.horizontal(|ui|{ui.label("Name");ui.text_edit_singleline(&mut form.name);});egui::ComboBox::from_label("Complete name / prefix").selected_text("Choose…").show_ui(ui,|ui|{for record in &self.records{if ui.selectable_label(false,&record.branch.name).clicked(){form.name=record.branch.name.clone();}
if let Some((prefix,_))=record.branch.name.rsplit_once('/'){if ui.selectable_label(false,format!("{prefix}/…")).clicked(){form.name=format!("{prefix}/");}}}});ui.checkbox(&mut form.checkout,"Checkout after creation");ui.checkbox(&mut form.overwrite,"Overwrite existing branch (replace its tip)");},
 FormKind::Rename=>{ui.text_edit_singleline(&mut form.name);},
 FormKind::Merge=>{for (mode,label) in [(MergeMode::FastForward,"Fast-forward when possible"),(MergeMode::NoFastForward,"No fast-forward (--no-ff)"),(MergeMode::FastForwardOnly,"Fast-forward only"),(MergeMode::Squash,"Squash (leave staged)")]{ui.radio_value(&mut form.mode,mode,crate::i18n::tr_static(label));}ui.label("Commit message");ui.text_edit_multiline(&mut form.message);},
 FormKind::Track=>{egui::ComboBox::from_label("Tracked branch").selected_text(&form.upstream).show_ui(ui,|ui|{for record in &self.records{if record.branch.is_remote{ui.selectable_value(&mut form.upstream,record.branch.name.clone(),&record.branch.name);}}});},
 FormKind::Delete=>{ui.label(&form.message);ui.checkbox(&mut form.force,"Delete even when not merged");},
 FormKind::Tag=>{ui.label("Tag name");ui.text_edit_singleline(&mut form.name);ui.label("Target revision");ui.text_edit_singleline(&mut form.target);ui.label("Annotated tag message (optional)");ui.text_edit_multiline(&mut form.message);},
 FormKind::Revision=>{ui.label("Revision, commit or tag");ui.text_edit_singleline(&mut form.target);egui::ComboBox::from_label("Complete reference").selected_text("Choose…").show_ui(ui,|ui|{for name in self.records.iter().map(|record|record.branch.name.clone()).chain(repo.tags().unwrap_or_default().into_iter().map(|(tag,_)|format!("refs/tags/{tag}"))){if ui.selectable_label(false,&name).clicked(){form.target=name;}}});ui.label("Checkout of a revision or tag creates detached HEAD.");},
 FormKind::CheckoutChoice=>{if form.force{ui.label("Force checkout discards local changes and may replace untracked files. Current files are copied to Local History first; binary/large files may not be recoverable.");}else{ui.label("Check out with rebase onto the current branch?");ui.checkbox(&mut form.checkout,"Rebase checked-out branch onto current");}},
 FormKind::Push=>{if form.upstream.is_empty(){form.upstream=repo.branch_push_remote(&form.target).unwrap_or_default();}ui.label("Destination ref (branch name or refs/tags/name)");ui.text_edit_singleline(&mut form.name);egui::ComboBox::from_label("Remote").selected_text(&form.upstream).show_ui(ui,|ui|{for remote in repo.remotes(){ui.selectable_value(&mut form.upstream,remote.clone(),remote);}});},
 FormKind::DeleteTag=>{ui.label("Delete this local tag? Remote tags remain unchanged.");},
 }
 let valid=match form.kind{FormKind::New=>rustpy_vcs::is_valid_branch_name(form.name.trim())&&(form.overwrite||!self.records.iter().any(|record|!record.branch.is_remote&&record.branch.name==form.name.trim())),FormKind::Rename=>rustpy_vcs::is_valid_branch_name(form.name.trim()),FormKind::Tag=>!form.name.trim().is_empty(),FormKind::Revision=>!form.target.trim().is_empty(),FormKind::Delete=>form.force||self.records.iter().find(|record|record.branch.name==form.target).is_none_or(|record|record.unmerged==0),FormKind::Push|FormKind::Track=>!form.upstream.is_empty(),_=>true};
 if !valid{ui.label("Complete the fields or explicitly allow deletion of unmerged commits.");}
 ui.horizontal(|ui|{apply=ui.add_enabled(valid,egui::Button::new("Apply")).clicked();cancel=ui.button("Cancel").clicked();});
 });
        if apply {
            let form = self.form.take().unwrap();
            let command = match form.kind {
                FormKind::New => Command::Local(LocalAction::Create {
                    name: form.name.trim().into(),
                    start: form.target,
                    checkout: form.checkout,
                    overwrite: form.overwrite,
                }),
                FormKind::Rename => Command::Local(LocalAction::Rename {
                    old: form.target,
                    new: form.name.trim().into(),
                }),
                FormKind::Merge => Command::Local(LocalAction::Merge {
                    branch: form.target,
                    mode: form.mode,
                    message: form.message,
                }),
                FormKind::Track => Command::Local(LocalAction::Track {
                    branch: form.target,
                    upstream: Some(form.upstream),
                }),
                FormKind::Delete => Command::Local(LocalAction::Delete {
                    branch: form.target,
                    force: form.force,
                }),
                FormKind::Tag => Command::Local(LocalAction::Tag {
                    name: form.name.trim().into(),
                    target: form.target,
                    message: form.message,
                }),
                FormKind::Revision => Command::Local(LocalAction::CheckoutRevision(form.target)),
                FormKind::CheckoutChoice if form.force => {
                    Command::Local(LocalAction::ForceCheckout(form.target))
                }
                FormKind::CheckoutChoice if form.checkout => {
                    Command::Local(LocalAction::CheckoutRebase(form.target))
                }
                FormKind::CheckoutChoice => {
                    if self
                        .records
                        .iter()
                        .any(|record| record.branch.name == form.target && record.branch.is_remote)
                    {
                        Command::CheckoutRemote(form.target)
                    } else {
                        Command::Checkout(form.target)
                    }
                }
                FormKind::Push => Command::Network {
                    label: format!("Push {}", form.target),
                    args: vec![
                        "push".into(),
                        "--progress".into(),
                        "--".into(),
                        form.upstream,
                        format!(
                            "{}:{}",
                            &form.target,
                            if form.name.is_empty() {
                                &form.target
                            } else {
                                &form.name
                            }
                        ),
                    ],
                },
                FormKind::DeleteTag => Command::Local(LocalAction::DeleteTag(form.target)),
            };
            commands.push(command);
            self.invalidate();
        } else if cancel {
            self.form = None;
        }
    }
}

pub struct BranchJob {
    pub receiver: std::sync::mpsc::Receiver<Completion>,
    pub token: rustpy_core::CancellationToken,
    pub id: u64,
}
impl Drop for BranchJob {
    fn drop(&mut self) {
        self.token.cancel();
    }
}
impl Workspace {
    pub(crate) fn cancel_project(&mut self) {
        self.cancel_jobs();
        let pending = std::mem::take(&mut self.pending);
        *self = Self::default();
        self.pending = pending;
    }
    pub fn cancel_jobs(&self) {
        for job in &self.pending {
            job.token.cancel();
        }
    }
    pub fn job_stamp(&self) -> Vec<(u64, bool)> {
        self.pending
            .iter()
            .map(|job| (job.id, job.token.is_cancelled()))
            .collect()
    }
}
pub struct Completion {
    pub root: PathBuf,
    pub result: Result<String, String>,
    pub restore: Option<(String, String)>,
    pub recent: Option<String>,
    pub blocked: Option<String>,
}
impl super::GitPanel {
    pub(super) fn branch_comparison_ui(
        &mut self,
        ui: &mut egui::Ui,
        events: &mut Vec<super::GitEvent>,
        root: &std::path::Path,
    ) {
        if self.branch_workspace.root != root {
            self.branch_workspace.comparison = None;
            return;
        }
        let Some(comparison) = &self.branch_workspace.comparison else {
            return;
        };
        let mut close = false;
        ui.horizontal(|ui| {
            ui.strong(format!(
                "Compare branches: {} ↔ {}",
                comparison.current, comparison.selected
            ));
            close = ui.small_button("Close comparison").clicked();
        });
        ui.columns(2, |columns| {
            for (index, (title, commits, more)) in [
                (
                    "Only in current",
                    &comparison.only_current,
                    comparison.current_more,
                ),
                (
                    "Only in selected",
                    &comparison.only_selected,
                    comparison.selected_more,
                ),
            ]
            .into_iter()
            .enumerate()
            {
                let ui = &mut columns[index];
                ui.strong(title);
                egui::ScrollArea::vertical()
                    .id_salt(("branch-compare", index))
                    .max_height(160.0)
                    .show(ui, |ui| {
                        for commit in commits {
                            if ui
                                .selectable_label(
                                    false,
                                    format!("{} {}", commit.short_id, commit.summary),
                                )
                                .on_hover_text(&commit.message)
                                .clicked()
                            {
                                events.push(super::GitEvent::ShowCommit(commit.id.clone()));
                            }
                        }
                        if more {
                            ui.label("More commits omitted");
                        }
                    });
            }
        });
        ui.separator();
        if close {
            self.branch_workspace.comparison = None;
        }
    }
}

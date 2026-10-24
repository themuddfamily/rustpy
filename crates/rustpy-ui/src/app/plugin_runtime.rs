//! Trusted, explicitly granted executable plugins. All publication belongs to
//! the captured workspace/document epoch; guest results never name host paths.
use super::*;
use crate::a11y::DialogWindow;
use rustpy_plugins::runtime::{self, Request, Response};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::time::{Duration, Instant};

#[derive(Clone)]
struct Invocation {
    plugin: String,
    event: String,
    action: Option<String>,
}
struct Pending {
    task: rustpy_core::TaskHandle<Response>,
    plugin: String,
    event: String,
    root: PathBuf,
    workspace: u64,
    document: Option<(rustpy_text::DocumentId, u64, usize)>,
    grant: String,
    authority: String,
}
#[derive(Default)]
struct Entry {
    response: Response,
    document: Option<(rustpy_text::DocumentId, u64, usize)>,
}
#[derive(Default)]
pub(super) struct State {
    pub open: bool,
    pub scripting: bool,
    pending: Option<Pending>,
    queue: VecDeque<Invocation>,
    entries: BTreeMap<String, Entry>,
    closed_windows: BTreeSet<(String, String)>,
    owner: Option<(PathBuf, u64)>,
    registry: Vec<String>,
    document: Option<(rustpy_text::DocumentId, u64, usize)>,
    due: Option<Instant>,
    mapped: BTreeMap<rustpy_text::DocumentId, (Language, Language)>,
    message: String,
    publisher_id: String,
    publisher_key: String,
    pub(super) script: super::plugin_scripting::State,
    pub(super) vim: super::plugin_vim::State,
}
impl Drop for State {
    fn drop(&mut self) {
        if let Some(pending) = &self.pending {
            pending.task.cancel();
        }
    }
}

impl IdeApp {
    pub(super) fn builtin_plugin_enabled(&self, id: &str) -> bool {
        self.settings.plugins.builtin_enabled(id)
    }
    pub(super) fn plugin_jobs(&self) -> Vec<(u64, String)> {
        let mut jobs = self
            .plugin_runtime
            .pending
            .iter()
            .map(|pending| {
                (
                    pending.task.id(),
                    format!("Plugin {}: {}", pending.plugin, pending.event),
                )
            })
            .collect::<Vec<_>>();
        jobs.extend(self.plugin_script_jobs());
        jobs.extend(self.plugin_background_jobs());
        jobs
    }
    pub(super) fn stop_plugin_job(&mut self, id: u64) -> bool {
        if let Some(pending) = self
            .plugin_runtime
            .pending
            .as_ref()
            .filter(|pending| pending.task.id() == id)
        {
            pending.task.cancel();
            self.plugin_runtime.queue.clear();
            return true;
        }
        self.stop_plugin_script(id) || self.stop_plugin_background_job(id)
    }
    /// Poll-only retirement for the shared managed-quit owner. No dialogs,
    /// settings writes, new tasks or publication while the quit guard is frozen.
    pub(super) fn retire_plugin_runtime(&mut self, ctx: &egui::Context) -> bool {
        self.plugin_runtime.queue.clear();
        if let Some(pending) = self.plugin_runtime.pending.as_mut() {
            pending.task.cancel();
            if pending.task.try_result().is_some() {
                self.plugin_runtime.pending = None;
            }
        }
        let script = self.retire_plugin_script(ctx);
        let background = self.retire_plugin_background_jobs(ctx);
        let busy = self.plugin_runtime.pending.is_some() || script || background;
        if busy {
            ctx.request_repaint_after(Duration::from_millis(25));
        }
        busy
    }
    pub(super) fn cancel_plugin_runtime(&mut self) {
        if let Some(pending) = self.plugin_runtime.pending.as_ref() {
            pending.task.cancel();
        }
        self.stop_all_plugin_scripts();
        self.plugin_runtime.queue.clear();
        self.plugin_runtime.registry.clear();
        self.plugin_runtime.entries.clear();
        self.plugin_runtime.document = None;
        for tab in &mut self.tabs {
            tab.state.plugin_diagnostics = None;
            if let Some((previous, mapped)) =
                self.plugin_runtime.mapped.remove(&tab.state.doc.identity())
            {
                if tab.state.language == mapped {
                    tab.state.set_language(previous);
                }
            }
        }
        if self.plugin_runtime.owner.take().is_some() {
            crate::plugin_host::clear_runtime_styles();
            self.apply_plugin_changes();
        }
    }
    fn plugin_document(&self) -> Option<runtime::Document> {
        let tab = self.active.and_then(|index| self.tabs.get(index))?;
        if usize::from(tab.state.doc.len()) > 512 * 1024 {
            return None;
        }
        Some(runtime::Document {
            path: tab
                .state
                .path
                .as_ref()
                .map(|path| path.display().to_string())
                .unwrap_or_default(),
            source: tab.state.doc.text(),
            version: tab.state.doc.version(),
            offset: usize::from(tab.state.primary().head()),
        })
    }
    pub(super) fn queue_plugin_action(&mut self, plugin: &str, action: &str) {
        if self.project_safe_mode() {
            self.notify_safe_mode("Executable plugins");
            return;
        }
        if !self
            .plugin_runtime
            .entries
            .get(plugin)
            .is_some_and(|entry| entry.response.actions.iter().any(|item| item.id == action))
        {
            return;
        }
        if plugin == "ide-script" {
            self.run_ide_script(Some(action.to_string()));
            return;
        }
        self.queue_plugin_invocation(plugin, "action", Some(action.into()));
    }
    fn queue_plugin_invocation(&mut self, plugin: &str, event: &str, action: Option<String>) {
        if self.plugin_runtime.queue.len() >= 32 {
            return;
        }
        self.plugin_runtime
            .queue
            .retain(|queued| queued.plugin != plugin || queued.event != event || event == "action");
        self.plugin_runtime.queue.push_back(Invocation {
            plugin: plugin.into(),
            event: event.into(),
            action,
        });
    }
    pub(super) fn plugin_runtime_tick(&mut self, ctx: &egui::Context) {
        if !self.builtin_plugin_enabled("git") {
            self.git_panel.cancel_log_project();
            self.git_panel.cancel_commit_project();
            self.git_panel.cancel_operation_project();
            self.git_panel.branch_workspace.cancel_project();
        }
        if !self.builtin_plugin_enabled("database") {
            self.db_panel.set_database_execution_allowed(false);
            self.db_panel.stop_all_database();
        }
        if !self.builtin_plugin_enabled("jupyter") {
            for tab in &mut self.tabs {
                if let Some(notebook) = tab.notebook.as_mut() {
                    notebook.stop_quit_process();
                }
            }
        }
        if self.project_safe_mode() || self.project.is_none() {
            if self.plugin_runtime.owner.is_some() || self.plugin_runtime.pending.is_some() {
                self.cancel_plugin_runtime();
            }
            if self
                .plugin_runtime
                .pending
                .as_mut()
                .is_some_and(|pending| pending.task.try_result().is_some())
            {
                self.plugin_runtime.pending = None;
            }
            if self.plugin_runtime.pending.is_some() {
                ctx.request_repaint_after(Duration::from_millis(25));
            }
            self.poll_plugin_script(ctx);
            return;
        }
        let owner = self
            .project
            .as_ref()
            .map(|project| (project.root.clone(), self.file_session));
        if self.plugin_runtime.owner != owner {
            self.cancel_plugin_runtime();
            self.plugin_runtime.owner = owner;
        }
        self.poll_plugin_script(ctx);
        let result = self
            .plugin_runtime
            .pending
            .as_mut()
            .and_then(|pending| pending.task.try_result());
        if let Some(result) = result {
            let pending = self.plugin_runtime.pending.take().unwrap();
            let current = !pending.task.is_cancelled()
                && pending.authority == publisher_authority(&self.settings.plugins)
                && pending.workspace == self.file_session
                && self
                    .project
                    .as_ref()
                    .is_some_and(|project| project.root == pending.root)
                && self.settings.plugins.runtime_grants.get(&pending.plugin)
                    == Some(&pending.grant)
                && crate::plugin_host::registry()
                    .plugins()
                    .iter()
                    .any(|plugin| plugin.id == pending.plugin && plugin.is_active());
            if current {
                match result {
                    Ok(response) => self.publish_plugin_response(
                        &pending.plugin,
                        &pending.event,
                        pending.document,
                        response,
                    ),
                    Err(error) => {
                        self.plugin_runtime.entries.remove(&pending.plugin);
                        self.plugin_runtime.message = format!("{}: {error}", pending.plugin);
                        self.publish_plugin_diagnostics();
                    }
                }
            }
        }
        let registry = crate::plugin_host::registry();
        let eligible = registry
            .plugins()
            .iter()
            .filter(|plugin| {
                plugin.is_active()
                    && plugin.manifest.runtime.is_some()
                    && self
                        .settings
                        .plugins
                        .runtime_grants
                        .contains_key(&plugin.id)
            })
            .map(|plugin| plugin.id.clone())
            .collect::<Vec<_>>();
        let mut fingerprint = eligible
            .iter()
            .filter_map(|id| registry.plugins().iter().find(|plugin| &plugin.id == id))
            .map(|plugin| {
                format!(
                    "{}:{}:{}",
                    plugin.id,
                    plugin.manifest.version,
                    self.settings.plugins.runtime_grants[&plugin.id]
                )
            })
            .collect::<Vec<_>>();
        fingerprint
            .push(serde_json::to_string(&self.settings.plugins.publishers).unwrap_or_default());
        fingerprint.push(self.settings.plugins.require_signatures.to_string());
        if self.plugin_runtime.registry != fingerprint {
            if let Some(pending) = self.plugin_runtime.pending.as_ref() {
                pending.task.cancel();
            }
            self.plugin_runtime.queue.clear();
            self.plugin_runtime
                .entries
                .retain(|id, _| id == "ide-script");
            self.plugin_runtime.registry = fingerprint;
            crate::plugin_host::clear_runtime_styles();
            self.apply_plugin_changes();
            for tab in &mut self.tabs {
                if let Some((previous, mapped)) =
                    self.plugin_runtime.mapped.remove(&tab.state.doc.identity())
                {
                    if tab.state.language == mapped {
                        tab.state.set_language(previous);
                    }
                }
            }
            self.publish_plugin_diagnostics();
            for id in &eligible {
                self.queue_plugin_invocation(id, "activate", None);
            }
            self.plugin_runtime.document = None;
        }
        let document = self
            .active
            .and_then(|index| self.tabs.get(index))
            .map(|tab| {
                (
                    tab.state.doc.identity(),
                    tab.state.doc.version(),
                    usize::from(tab.state.primary().head()),
                )
            });
        if self.plugin_runtime.document != document {
            self.apply_plugin_file_types();
            self.plugin_runtime.document = document;
            self.plugin_runtime.due = Some(Instant::now() + Duration::from_millis(300));
        }
        if self
            .plugin_runtime
            .due
            .is_some_and(|due| Instant::now() >= due)
        {
            self.plugin_runtime.due = None;
            for id in &eligible {
                self.queue_plugin_invocation(id, "document", None);
            }
        }
        if self.plugin_runtime.pending.is_none() {
            if let Some(invocation) = self.plugin_runtime.queue.pop_front() {
                if let Some(plugin) = registry
                    .plugins()
                    .iter()
                    .find(|plugin| plugin.id == invocation.plugin && plugin.is_active())
                {
                    if let (Some(manifest), Some(grant)) = (
                        plugin.manifest.runtime.clone(),
                        self.settings
                            .plugins
                            .runtime_grants
                            .get(&plugin.id)
                            .cloned(),
                    ) {
                        let dir = plugin.dir.clone();
                        let document = self.plugin_document();
                        let captured = document.as_ref().and_then(|document| {
                            self.active
                                .and_then(|index| self.tabs.get(index))
                                .map(|tab| {
                                    (tab.state.doc.identity(), document.version, document.offset)
                                })
                        });
                        let request = Request {
                            api_version: 1,
                            event: invocation.event.clone(),
                            action: invocation.action,
                            document,
                        };
                        let authority = publisher_authority(&self.settings.plugins);
                        let policy = self.settings.plugins.clone();
                        let expected = grant.clone();
                        let task = rustpy_core::TaskManager::global()
                            .task(format!("Plugin {}", plugin.name()))
                            .spawn(move |progress| {
                                progress.check()?;
                                runtime::invoke(&dir, &manifest, &expected, request, &policy)
                                    .map_err(rustpy_core::TaskError::Failed)
                            });
                        self.plugin_runtime.pending = Some(Pending {
                            task,
                            plugin: plugin.id.clone(),
                            event: invocation.event,
                            root: self.project.as_ref().unwrap().root.clone(),
                            workspace: self.file_session,
                            document: captured,
                            grant,
                            authority,
                        });
                    }
                }
            }
        }
        if self.plugin_runtime.pending.is_some() || self.plugin_runtime.due.is_some() {
            ctx.request_repaint_after(Duration::from_millis(50));
        }
    }
    pub(super) fn publish_plugin_response(
        &mut self,
        plugin: &str,
        event: &str,
        document: Option<(rustpy_text::DocumentId, u64, usize)>,
        response: Response,
    ) {
        let current = document.and_then(|(id, version, _)| {
            self.tabs.iter().position(|tab| {
                tab.state.doc.identity() == id && tab.state.doc.version() == version
            })
        });
        if document.is_some() && current.is_none() {
            return;
        }
        if !response.edits.is_empty() && matches!(event, "action" | "script") {
            if let Some(index) = current {
                let edits = response
                    .edits
                    .iter()
                    .map(|edit| {
                        (
                            TextRange::new(
                                TextSize::new(edit.start as u32),
                                TextSize::new(edit.end as u32),
                            ),
                            edit.text.clone(),
                        )
                    })
                    .collect();
                self.tabs[index]
                    .state
                    .apply_text_edits(edits, &format!("Plugin {plugin}"));
            }
        }
        let styles = !response.themes.is_empty() || !response.keymaps.is_empty();
        if styles {
            match crate::plugin_host::set_runtime_styles(plugin, &response) {
                Ok(()) => {
                    self.apply_plugin_changes();
                }
                Err(error) => self.plugin_runtime.message = error,
            }
        }
        if !response.status.is_empty() {
            self.set_status(response.status.clone());
        }
        let entry = self
            .plugin_runtime
            .entries
            .entry(plugin.into())
            .or_default();
        if !response.actions.is_empty() {
            entry.response.actions = response.actions.clone();
        }
        if !response.tool_windows.is_empty() {
            entry.response.tool_windows = response.tool_windows.clone();
        }
        if !response.file_types.is_empty() {
            entry.response.file_types = response.file_types;
        }
        if document.is_some() {
            entry.document = document;
            entry.response.diagnostics = response.diagnostics;
            entry.response.completions = response.completions;
        }
        self.apply_plugin_file_types();
        self.publish_plugin_diagnostics();
    }
    fn apply_plugin_file_types(&mut self) {
        for tab in &mut self.tabs {
            if !matches!(tab.state.language, Language::Other | Language::PlainText) {
                continue;
            }
            let Some(extension) = tab
                .state
                .path
                .as_ref()
                .and_then(|path| path.extension())
                .and_then(|extension| extension.to_str())
            else {
                continue;
            };
            let mapped = self
                .plugin_runtime
                .entries
                .values()
                .flat_map(|entry| &entry.response.file_types)
                .find(|kind| {
                    kind.extensions.iter().any(|candidate| {
                        candidate
                            .trim_start_matches('.')
                            .eq_ignore_ascii_case(extension)
                    })
                })
                .and_then(|kind| plugin_language(&kind.language));
            if let Some(mapped) = mapped {
                self.plugin_runtime
                    .mapped
                    .entry(tab.state.doc.identity())
                    .or_insert((tab.state.language, mapped));
                tab.state.set_language(mapped);
            }
        }
    }
    pub(super) fn remove_script_results(&mut self) {
        if self.plugin_runtime.entries.remove("ide-script").is_some() {
            self.publish_plugin_diagnostics();
        }
    }
    pub(super) fn publish_plugin_diagnostics(&mut self) {
        for tab in &mut self.tabs {
            let diagnostics = self
                .plugin_runtime
                .entries
                .iter()
                .filter(|(_, entry)| {
                    entry.document.is_some_and(|(id, version, _)| {
                        id == tab.state.doc.identity() && version == tab.state.doc.version()
                    })
                })
                .flat_map(|(plugin, entry)| {
                    entry.response.diagnostics.iter().map(move |diagnostic| {
                        rustpy_core::Diagnostic::new(
                            TextRange::new(
                                TextSize::new(diagnostic.start as u32),
                                TextSize::new(diagnostic.end as u32),
                            ),
                            match diagnostic.severity.as_str() {
                                "error" => rustpy_core::Severity::Error,
                                "warning" => rustpy_core::Severity::Warning,
                                _ => rustpy_core::Severity::Info,
                            },
                            format!("plugin.{plugin}.{}", diagnostic.code),
                            diagnostic.message.clone(),
                        )
                    })
                })
                .collect();
            tab.state.plugin_diagnostics = Some((tab.state.doc.version(), diagnostics));
        }
    }
    pub(super) fn append_plugin_completions(
        &self,
        index: usize,
        prefix: &str,
        items: &mut Vec<CompletionItem>,
        imports: &mut Vec<Option<(String, String)>>,
    ) {
        if self.project_safe_mode() {
            return;
        }
        let Some(tab) = self.tabs.get(index) else {
            return;
        };
        for entry in self.plugin_runtime.entries.values().filter(|entry| {
            entry.document
                == Some((
                    tab.state.doc.identity(),
                    tab.state.doc.version(),
                    usize::from(tab.state.primary().head()),
                ))
        }) {
            for completion in entry
                .response
                .completions
                .iter()
                .filter(|completion| completion.label.starts_with(prefix))
            {
                if items
                    .iter()
                    .any(|item| item.label.as_str() == completion.label)
                {
                    continue;
                }
                items.push(CompletionItem {
                    label: completion.label.clone().into(),
                    kind: CompletionKind::Keyword,
                    detail: completion.detail.clone(),
                    insert: completion.insert.clone(),
                    caret_offset: None,
                    priority: 120,
                });
                imports.push(None);
            }
        }
    }
    pub(super) fn open_plugin_completion(&mut self, explicit: bool) -> bool {
        let Some(index) = self.active else {
            return false;
        };
        let tab = &self.tabs[index];
        if tab.state.language == Language::Python {
            return false;
        }
        let source = tab.state.doc.text();
        let caret = tab.state.primary().head();
        let end = usize::from(caret).min(source.len());
        let start = source[..end]
            .rfind(|character: char| !rustpy_core::is_ident_continue(character))
            .map(|offset| {
                offset
                    + source[offset..]
                        .chars()
                        .next()
                        .map(char::len_utf8)
                        .unwrap_or(0)
            })
            .unwrap_or(0);
        let mut items = Vec::new();
        let mut imports = Vec::new();
        self.append_plugin_completions(index, &source[start..end], &mut items, &mut imports);
        if items.is_empty() {
            return false;
        }
        let version = tab.state.doc.version();
        self.cancel_basic_completion();
        self.import_completion = None;
        self.completion = CompletionPopup {
            visible: true,
            items,
            imports,
            selected: 0,
            prefix_start: TextSize::new(start as u32),
            doc_version: version,
            caret,
            explicit,
            rect: None,
        };
        true
    }
    pub(super) fn plugin_runtime_menu(&mut self, ui: &mut egui::Ui) {
        if ui.button("Executable Plugins / IDE Scripting…").clicked() {
            self.plugin_runtime.open = true;
            ui.close();
        }
        let actions =
            self.plugin_runtime
                .entries
                .iter()
                .flat_map(|(plugin, entry)| {
                    entry.response.actions.iter().map(move |action| {
                        (plugin.clone(), action.id.clone(), action.title.clone())
                    })
                })
                .collect::<Vec<_>>();
        let windows =
            self.plugin_runtime
                .entries
                .iter()
                .flat_map(|(plugin, entry)| {
                    entry.response.tool_windows.iter().map(move |window| {
                        (plugin.clone(), window.id.clone(), window.title.clone())
                    })
                })
                .collect::<Vec<_>>();
        for (plugin, id, title) in windows {
            if ui.button(format!("Open {plugin}: {title}")).clicked() {
                self.plugin_runtime.closed_windows.remove(&(plugin, id));
                ui.close();
            }
        }
        for (plugin, id, title) in actions {
            if ui.button(format!("{plugin}: {title}")).clicked() {
                self.queue_plugin_action(&plugin, &id);
                ui.close();
            }
        }
    }
    pub(super) fn plugin_runtime_windows(&mut self, ctx: &egui::Context) {
        let mut open = self.plugin_runtime.open;
        if open {
            egui::Window::new("Executable Plugins / IDE Scripting").open(&mut open).default_width(680.).show_modeless(ctx,|ui|{
                ui.label("WASM plugins have no filesystem, process, network or environment imports. Review the requested editor capabilities before granting a specific module version.");
                if self.project_safe_mode(){ui.colored_label(self.theme.warning,"Trust the project before executing plugins or scripts.");}
                let registry=crate::plugin_host::registry();
                for plugin in registry.plugins().iter().filter(|plugin|plugin.manifest.runtime.is_some()) {
                    let manifest=plugin.manifest.runtime.as_ref().unwrap();
                    ui.group(|ui|{
                        ui.strong(format!("{} {}",plugin.name(),plugin.manifest.version));
                        ui.label(format!("Requested capabilities: {:?}",manifest.capabilities));
                        let granted=self.settings.plugins.runtime_grants.contains_key(&plugin.id);
                        if ui.add_enabled(!granted&&plugin.is_active()&&!self.project_safe_mode(),egui::Button::new("Review and grant these capabilities")).clicked(){
                            match runtime::digest(&plugin.dir,manifest).and_then(|digest|rustpy_plugins::signatures::verify_directory(&plugin.dir,&self.settings.plugins).map(|_|digest)) {
                                Ok(digest)=>{self.settings.plugins.runtime_grants.insert(plugin.id.clone(),digest);let _=self.settings.save();}
                                Err(error)=>self.plugin_runtime.message=error,
                            }
                        }
                        if granted && ui.button("Revoke executable permission").clicked(){self.settings.plugins.runtime_grants.remove(&plugin.id);let _=self.settings.save();}
                    });
                }
                ui.collapsing("Required and suggested project plugins",|ui|{
                    if let Some(project)=&self.project {
                        match rustpy_plugins::requirements::Requirements::load(&project.root){
                            Ok(requirements)=>{
                                let missing=requirements.missing(&registry);
                                if missing.is_empty(){ui.label("All required plugins are available.");}
                                for requirement in missing {ui.colored_label(self.theme.warning,format!("Required: {requirement}"));}
                                let extension=self.active.and_then(|index|self.tabs.get(index)).and_then(|tab|tab.state.path.as_ref()).and_then(|path|path.extension()).and_then(|extension|extension.to_str()).unwrap_or_default().to_ascii_lowercase();
                                for plugin in self.suggested_plugins_for_extension(&extension){ui.label(format!("Marketplace suggestion for .{extension}: {plugin}"));}
                                for (pattern,plugins) in &requirements.suggested {if pattern.trim_start_matches('.').eq_ignore_ascii_case(&extension){for plugin in plugins{ui.label(format!("Suggested for .{extension}: {plugin}"));}}}
                            }
                            Err(error)=>{ui.colored_label(self.theme.warning,error);}
                        }
                    }
                    ui.label("Project .rustpy/plugins.json requirements are advisory; installation and executable permissions require your approval.");
                    if ui.button("Review in Marketplace").clicked(){self.settings_ui.plugins.tab=super::plugins_ui::PluginsTab::Marketplace;self.settings_ui.page=super::settings_ui::SettingsPage::Plugins;self.dialog=super::Dialog::Settings;}
                });
                ui.collapsing("Bundled capabilities",|ui|{
                    for (id,title) in rustpy_settings::plugins::BUILTINS {
                        let mut enabled=self.settings.plugins.builtin_enabled(id);
                        if ui.checkbox(&mut enabled,*title).changed(){
                            if enabled{self.settings.plugins.disabled_builtins.remove(*id);}else{self.settings.plugins.disabled_builtins.insert((*id).into());}
                            let _=self.settings.save();
                        }
                    }
                });
                ui.collapsing("Publisher signatures",|ui|{
                    if ui.checkbox(&mut self.settings.plugins.require_signatures,"Require a trusted Ed25519 publisher signature for all plugins").changed(){let _=self.settings.save();crate::plugin_host::reload();self.apply_plugin_changes();}
                    let mut remove=None;
                    for publisher in self.settings.plugins.publishers.keys(){ui.horizontal(|ui|{ui.label(publisher);if ui.button("Remove publisher").clicked(){remove=Some(publisher.clone());}});}
                    if let Some(publisher)=remove{self.settings.plugins.publishers.remove(&publisher);let _=self.settings.save();crate::plugin_host::reload();self.apply_plugin_changes();}
                    ui.text_edit_singleline(&mut self.plugin_runtime.publisher_id);
                    ui.add(egui::TextEdit::singleline(&mut self.plugin_runtime.publisher_key).hint_text("Base64 Ed25519 public verification key"));
                    if ui.button("Trust publisher key").clicked(){
                        use base64::Engine;
                        let id=self.plugin_runtime.publisher_id.trim().to_string();let key=self.plugin_runtime.publisher_key.trim().to_string();
                        if !id.is_empty()&&id.len()<=128&&base64::engine::general_purpose::STANDARD.decode(&key).is_ok_and(|bytes|bytes.len()==32){
                            self.settings.plugins.publishers.insert(id,key);let _=self.settings.save();crate::plugin_host::reload();self.apply_plugin_changes();
                        }else{self.plugin_runtime.message="Enter a publisher id and a 32-byte Ed25519 public key".into();}
                    }
                });
                if ui.checkbox(&mut self.settings.plugins.vim_enabled,"Vim-style modal editing (bounded command set)").changed(){let _=self.settings.save();}
                ui.label("Modal editing: Normal/Insert modes, h/j/k/l, 0/$, x, i/a, u, Ctrl+R, :w and :q. Other Vim commands are not implemented.");
                if ui.button("IDE Scripting Console").clicked(){self.plugin_runtime.scripting=true;}
                if !self.plugin_runtime.message.is_empty(){ui.label(&self.plugin_runtime.message);}
            });
        }
        self.plugin_runtime.open = open;
        let windows = self
            .plugin_runtime
            .entries
            .iter()
            .flat_map(|(plugin, entry)| {
                entry
                    .response
                    .tool_windows
                    .iter()
                    .map(move |window| (plugin.clone(), window.clone()))
            })
            .collect::<Vec<_>>();
        for (plugin, window) in windows {
            let key = (plugin.clone(), window.id.clone());
            if self.plugin_runtime.closed_windows.contains(&key) {
                continue;
            }
            let mut visible = true;
            egui::Window::new(format!("{}: {}", plugin, window.title))
                .open(&mut visible)
                .id(egui::Id::new(("plugin-window", &plugin, &window.id)))
                .show_modeless(ctx, |ui| {
                    ui.label(&window.text);
                    for id in &window.actions {
                        if ui
                            .add_enabled(!self.project_safe_mode(), egui::Button::new(id))
                            .clicked()
                        {
                            self.queue_plugin_action(&plugin, id);
                        }
                    }
                });
            if !visible {
                self.plugin_runtime.closed_windows.insert(key);
            }
        }
        self.plugin_scripting_window(ctx);
    }
}

fn plugin_language(language: &str) -> Option<Language> {
    Some(match language.to_ascii_lowercase().as_str() {
        "python" => Language::Python,
        "json" => Language::Json,
        "yaml" => Language::Yaml,
        "toml" => Language::Toml,
        "javascript" => Language::JavaScript,
        "html" => Language::Html,
        "css" => Language::Css,
        "markdown" => Language::Markdown,
        "shell" => Language::Shell,
        "sql" => Language::Sql,
        "text" => Language::PlainText,
        _ => return None,
    })
}

fn publisher_authority(settings: &rustpy_settings::plugins::Settings) -> String {
    format!(
        "{}:{}",
        settings.require_signatures,
        serde_json::to_string(&settings.publishers).unwrap_or_default()
    )
}

#[cfg(test)]
mod configured_execution_tests {
    use super::*;
    use rustpy_plugins::runtime::{Capability, RuntimeManifest};

    const ID: &str = "org.rustpy.actual-wasm-acceptance";

    fn leb(mut value: u64) -> Vec<u8> {
        let mut bytes = Vec::new();
        loop {
            let low = (value & 127) as u8;
            value >>= 7;
            bytes.push(low | if value == 0 { 0 } else { 128 });
            if value == 0 {
                return bytes;
            }
        }
    }
    fn signed(mut value: i64) -> Vec<u8> {
        let mut bytes = Vec::new();
        loop {
            let low = (value & 127) as u8;
            value >>= 7;
            let done = (value == 0 && low & 64 == 0) || (value == -1 && low & 64 != 0);
            bytes.push(low | if done { 0 } else { 128 });
            if done {
                return bytes;
            }
        }
    }
    fn section(wasm: &mut Vec<u8>, id: u8, bytes: &[u8]) {
        wasm.push(id);
        wasm.extend(leb(bytes.len() as u64));
        wasm.extend(bytes);
    }
    fn guest(base: &serde_json::Value, action: &serde_json::Value) -> Vec<u8> {
        // An actual import-free guest: allocator returns a separate request
        // buffer; invoke reads the serialized event and returns guest-owned
        // UTF-8 JSON from its data segment. It never supplies host Rust values.
        let base = serde_json::to_vec(base).unwrap();
        let action = serde_json::to_vec(action).unwrap();
        assert!(base.len() < 4096 && action.len() < 4096);
        let request = serde_json::to_vec(&Request {
            api_version: 1,
            event: "action".into(),
            action: None,
            document: None,
        })
        .unwrap();
        let marker = b"\"event\":\"action\"";
        let event_byte = request
            .windows(marker.len())
            .position(|part| part == marker)
            .unwrap()
            + b"\"event\":\"".len()
            + 4;
        let mut wasm = b"\0asm\x01\0\0\0".to_vec();
        section(
            &mut wasm,
            1,
            &[2, 0x60, 1, 0x7f, 1, 0x7f, 0x60, 2, 0x7f, 0x7f, 1, 0x7e],
        );
        section(&mut wasm, 3, &[2, 0, 1]);
        section(&mut wasm, 5, &[1, 0, 1]);
        let mut exports = vec![3];
        for (name, kind, index) in [
            ("memory", 2, 0),
            ("rustpy_alloc", 0, 0),
            ("rustpy_invoke", 0, 1),
        ] {
            exports.extend(leb(name.len() as u64));
            exports.extend(name.as_bytes());
            exports.extend([kind, index]);
        }
        section(&mut wasm, 7, &exports);
        let mut allocator = vec![0, 0x41];
        allocator.extend(signed(16384));
        allocator.push(0x0b);
        let mut invoke = vec![0, 0x20, 0, 0x2d, 0];
        invoke.extend(leb(event_byte as u64));
        invoke.push(0x41);
        invoke.extend(signed(i64::from(b'o')));
        invoke.extend([0x46, 0x04, 0x7e, 0x42]);
        invoke.extend(signed((8192i64 << 32) | action.len() as i64));
        invoke.extend([0x05, 0x42]);
        invoke.extend(signed((1024i64 << 32) | base.len() as i64));
        invoke.extend([0x0b, 0x0b]);
        let mut code = vec![2];
        for body in [allocator, invoke] {
            code.extend(leb(body.len() as u64));
            code.extend(body);
        }
        section(&mut wasm, 10, &code);
        let mut data = vec![2];
        for (offset, bytes) in [(1024, base), (8192, action)] {
            data.extend([0, 0x41]);
            data.extend(signed(offset));
            data.push(0x0b);
            data.extend(leb(bytes.len() as u64));
            data.extend(bytes);
        }
        section(&mut wasm, 11, &data);
        wasm
    }
    struct Installed;
    impl Drop for Installed {
        fn drop(&mut self) {
            let _ = crate::plugin_host::update(|registry| registry.uninstall(ID));
        }
    }
    fn settle(app: &mut IdeApp, ctx: &egui::Context) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            app.plugin_runtime_tick(ctx);
            if app.plugin_runtime.pending.is_none() && app.plugin_runtime.queue.is_empty() {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "actual guest did not settle: {}",
                app.plugin_runtime.message
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    #[ignore = "explicit admitted installed-WASM/App consumer acceptance; serial isolated config required"]
    fn actual_wasm_registry_guest_publishes_app_consumers_and_rejects_stale_revoked_permissions() {
        assert_eq!(
            std::env::var("RUSTPY_REQUIRE_PROVIDER_TESTS").as_deref(),
            Ok("1")
        );
        let directory = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(directory.path()).unwrap();
        let source = root.join("plugin");
        std::fs::create_dir(&source).unwrap();
        let base = serde_json::json!({"api_version":1,
            "actions":[{"id":"actual.replace","title":"Actual guest replace"}],
            "tool_windows":[{"id":"actual.window","title":"Actual guest window","text":"Guest-produced window body","actions":["actual.replace"]}],
            "diagnostics":[{"start":0,"end":1,"severity":"warning","code":"ActualGuest","message":"Guest-produced diagnostic"}],
            "completions":[{"label":"guest_value","insert":"guest_value","detail":"Guest-produced completion"}],
            "file_types":[{"extensions":["actualguest"],"language":"json"}],
            "themes":[{"name":"Actual Guest Theme","base":"Dark","colors":{"Keyword":"#123456"}}],
            "keymaps":[{"name":"Actual Guest Keys","bindings":{"EditorLineStart":["Alt+Home"]}}],
            "status":"Actual Wasm publication"});
        let mut action = base.clone();
        action["edits"] = serde_json::json!([{"start":0,"end":1,"text":"y"}]);
        let manifest = RuntimeManifest {
            api_version: 1,
            module: "actual.wasm".into(),
            capabilities: [
                Capability::DocumentRead,
                Capability::DocumentWrite,
                Capability::Actions,
                Capability::ToolWindows,
                Capability::Inspections,
                Capability::Completion,
                Capability::FileTypes,
                Capability::Themes,
                Capability::Keymaps,
            ]
            .into_iter()
            .collect(),
        };
        let write_manifest = |manifest: &RuntimeManifest| {
            std::fs::write(source.join("plugin.json"), serde_json::json!({"id":ID,"name":"Actual Wasm Acceptance","version":"1.0.0","runtime":manifest}).to_string()).unwrap()
        };
        write_manifest(&manifest);
        std::fs::write(source.join("actual.wasm"), guest(&base, &action)).unwrap();
        assert!(
            crate::plugin_host::registry().get(ID).is_none(),
            "fixture ID must be isolated"
        );
        crate::plugin_host::update(|registry| registry.install_from_dir(&source)).unwrap();
        let _installed = Installed;
        let registry = crate::plugin_host::registry();
        let installed = registry.get(ID).unwrap();
        let grant = runtime::digest(&installed.dir, &manifest).unwrap();
        let mut settings = Settings::default();
        settings.trust_project(&root);
        settings.plugins.require_signatures = false;
        settings
            .plugins
            .runtime_grants
            .insert(ID.into(), grant.clone());
        let mut app = IdeApp::new(settings, Some(root.clone()));
        let mut state = EditorState::from_text("x", Language::PlainText);
        state.path = Some(root.join("sample.actualguest"));
        app.push_tab(state);
        let index = app.active.unwrap();
        let ctx = egui::Context::default();
        settle(&mut app, &ctx);
        assert_eq!(app.tabs[index].state.doc.text(), "x");
        assert_eq!(app.tabs[index].state.language, Language::Json);
        let diagnostics = &app.tabs[index].state.plugin_diagnostics.as_ref().unwrap().1;
        assert!(diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("Guest-produced diagnostic")));
        assert!(app.open_plugin_completion(true));
        assert!(app
            .completion
            .items
            .iter()
            .any(|item| item.label.as_str() == "guest_value"));
        assert!(crate::plugin_host::color_scheme("Actual Guest Theme").is_some());
        assert!(crate::plugin_host::keymap_names()
            .iter()
            .any(|name| name == "Actual Guest Keys"));
        app.scheme = super::super::settings_ui::scheme_by_name("Actual Guest Theme").unwrap();
        app.settings.editor.color_scheme = app.scheme.name.clone();
        app.settings.keymap = "Actual Guest Keys".into();
        app.apply_custom_shortcuts();
        assert_eq!(
            app.scheme.styles[&rustpy_highlight::HighlightTag::Keyword].fg,
            Some(rustpy_highlight::Rgb(0x12, 0x34, 0x56))
        );
        assert_eq!(
            app.keymap
                .lookup(crate::actions::KeyChord::parse("Alt+Home").unwrap()),
            Some(Action::CaretLineStart)
        );
        assert_eq!(
            app.plugin_runtime.entries[ID].response.tool_windows[0].text,
            "Guest-produced window body"
        );
        let mut painted = Vec::new();
        for frame in 0..4 {
            // A newly opened egui window first measures its content. Give it
            // bounded real frames before requiring the painted body text.
            let output = ctx.run_ui(
                egui::RawInput {
                    time: Some(f64::from(frame) / 60.0),
                    focused: true,
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(1200.0, 800.0),
                    )),
                    ..Default::default()
                },
                |ui| app.plugin_runtime_windows(ui.ctx()),
            );
            painted.extend(output.shapes.iter().filter_map(|shape| match &shape.shape {
                egui::Shape::Text(text) => Some(text.galley.text().to_owned()),
                _ => None,
            }));
            // Dispose before assertions, including their failure/unwind path.
            output.drop_without_applying_deltas();
            if painted
                .iter()
                .any(|text| text.contains("Guest-produced window body"))
            {
                break;
            }
        }
        assert!(
            painted
                .iter()
                .any(|text| text.contains("Guest-produced window body")),
            "actual window body was not painted after sizing frames: {painted:?}"
        );
        app.queue_plugin_action(ID, "actual.replace");
        settle(&mut app, &ctx);
        assert_eq!(app.tabs[index].state.doc.text(), "y");
        assert!(app.tabs[index].state.undo());
        assert_eq!(app.tabs[index].state.doc.text(), "x");

        let captured = (
            app.tabs[index].state.doc.identity(),
            app.tabs[index].state.doc.version(),
            0,
        );
        let request = Request {
            api_version: 1,
            event: "action".into(),
            action: Some("actual.replace".into()),
            document: app.plugin_document(),
        };
        let response = runtime::invoke(
            &installed.dir,
            &manifest,
            &grant,
            request.clone(),
            &app.settings.plugins,
        )
        .unwrap();
        app.tabs[index].state.apply_text_edits(
            vec![(
                TextRange::new(TextSize::new(0), TextSize::new(1)),
                "z".into(),
            )],
            "newer editor",
        );
        app.publish_plugin_response(ID, "action", Some(captured), response);
        assert_eq!(
            app.tabs[index].state.doc.text(),
            "z",
            "stale actual guest result must not overwrite newer text"
        );
        let mut denied = manifest.clone();
        denied.capabilities.remove(&Capability::DocumentWrite);
        write_manifest(&denied);
        let denied_grant = runtime::digest(&source, &denied).unwrap();
        assert!(runtime::invoke(
            &source,
            &denied,
            &denied_grant,
            request,
            &app.settings.plugins
        )
        .unwrap_err()
        .contains("DocumentWrite"));
        app.queue_plugin_action(ID, "actual.replace");
        app.plugin_runtime_tick(&ctx);
        assert!(app.plugin_runtime.pending.is_some());
        app.settings.plugins.runtime_grants.remove(ID);
        settle(&mut app, &ctx);
        assert_eq!(app.tabs[index].state.doc.text(), "z");
        assert!(!app.plugin_runtime.entries.contains_key(ID));
        assert!(crate::plugin_host::color_scheme("Actual Guest Theme").is_none());
        assert_ne!(app.scheme.name, "Actual Guest Theme");
        assert_ne!(app.settings.keymap, "Actual Guest Keys");
        assert_eq!(app.tabs[index].state.language, Language::PlainText);
        app.cancel_plugin_runtime();
        assert!(app.plugin_jobs().is_empty());
    }
}

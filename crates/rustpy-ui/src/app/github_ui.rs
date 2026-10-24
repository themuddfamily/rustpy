//! Connected GitHub account/review workflows. Workers capture account and root.
use super::*;
use crate::github_panel::{ComposeKind, Composer, Event, Workspace};
use base64::Engine;
use rustpy_core::{TaskHandle, TaskManager};
use rustpy_settings::github::{Account, Provider};
use rustpy_vcs::github::{self, Client, HttpTransport, Operation, Request, Server};
use serde_json::{json, Value};
#[derive(Clone, Debug)]
enum Data {
    Login(Account),
    Activated,
    DefaultBranch(String),
    Push {
        root: PathBuf,
        remote: String,
        branch: String,
    },
    Removed(String),
    Repositories(Vec<github::Repository>),
    Organizations(Vec<String>),
    Pulls(Vec<github::PullRequest>),
    Details(Box<github::PullDetails>),
    Notifications(Vec<github::Notification>),
    Written(Value),
    Shared {
        repository: Box<github::Repository>,
        branch: Option<String>,
        error: Option<String>,
        remote: String,
    },
}
struct Pending {
    task: TaskHandle<Data>,
    account: String,
    repository: String,
    root: Option<PathBuf>,
    operation: Option<Operation>,
    open_browser: bool,
    label: String,
}
#[derive(Default)]
pub(super) struct State {
    pub panel: Workspace,
    pending: Vec<Pending>,
    device: Option<std::sync::mpsc::Receiver<github::auth::DeviceCode>>,
    pub clone_directory: Option<PathBuf>,
    pub clone_picker: bool,
    gutter_stamp: Option<String>,
    open_urls: Vec<String>,
    checkout_after_fetch: Option<(PathBuf, String, String, String)>,
    latest: std::collections::HashMap<String, u64>,
}
impl Drop for State {
    fn drop(&mut self) {
        self.cancel();
    }
}
impl State {
    pub fn cancel(&self) {
        for pending in &self.pending {
            pending.task.cancel();
        }
    }
    pub fn busy(&self) -> bool {
        !self.pending.is_empty()
    }
    pub fn stamp(&self) -> Vec<String> {
        self.pending
            .iter()
            .map(|pending| {
                format!(
                    "{:?}:{}:{}",
                    pending.task.id(),
                    pending.account,
                    pending.task.is_cancelled()
                )
            })
            .collect()
    }
}
fn apply_git_account(account: &Account) -> Result<(), String> {
    let token = rustpy_secrets::get(&account.secret_key())
        .map_err(|error| error.to_string())?
        .ok_or("Account token is unavailable")?;
    let user = if account.provider == Provider::GitLab {
        "oauth2"
    } else {
        "x-access-token"
    };
    let encoded = base64::engine::general_purpose::STANDARD.encode(format!("{user}:{token}"));
    rustpy_vcs::policy::github_authorization_for(
        &account.server,
        format!("Authorization: Basic {encoded}"),
    )
    .map_err(|error| error.to_string())
}
fn task_error(error: impl ToString) -> rustpy_core::TaskError {
    rustpy_core::TaskError::Failed(error.to_string())
}
impl IdeApp {
    pub(super) fn github_accounts_page(&mut self, ui: &mut egui::Ui) {
        if self.settings.github.oauth_client_id.is_empty() {
            self.settings.github.oauth_client_id =
                self.settings.vcs.preferences.github_client_id.clone();
        }
        let busy = self.github.busy();
        let events = self
            .github
            .panel
            .account_ui(ui, &mut self.settings.github, busy);
        for event in events {
            self.handle_github_event(event);
        }
    }
    pub(super) fn github_panel_ui(&mut self, ui: &mut egui::Ui, theme: &UiTheme) {
        if !self.builtin_plugin_enabled("github") {
            ui.label("The bundled GitHub / GitLab plugin is disabled");
            return;
        }
        self.infer_github_repository();
        let busy = self.github.busy();
        let events = self.github.panel.ui(ui, theme, &self.settings.github, busy);
        for event in events {
            self.handle_github_event(event);
        }
        if self.github.panel.composer.is_some() {
            self.dialog = Dialog::GitHubModal;
        }
    }
    pub(super) fn github_dialog(&mut self, ctx: &egui::Context) {
        self.bind_github_composer();
        let busy = self.github.busy();
        let (open, events) = self.github.panel.dialog(ctx, &self.settings.github, busy);
        if open {
            self.dialog = Dialog::GitHubModal;
        }
        for event in events {
            self.handle_github_event(event);
        }
    }
    pub(super) fn open_github(&mut self) {
        self.infer_github_repository();
        self.bottom = Some(BottomTool::PullRequests);
        if self.settings.github.active().is_some() {
            self.handle_github_event(Event::Pulls);
        } else {
            self.open_settings_page("github");
        }
    }
    pub(super) fn open_github_repositories(&mut self, clone: bool) {
        self.github.clone_picker = clone;
        self.github.panel.composer = Some(Composer::new(ComposeKind::Repositories));
        self.dialog = Dialog::GitHubModal;
        if self.settings.github.active().is_some() {
            self.handle_github_event(Event::Repositories);
        }
    }
    fn infer_github_repository(&mut self) {
        let Some(account) = self.settings.github.active() else {
            return;
        };
        let Ok(server) = Server::new(&account.server, account.provider) else {
            return;
        };
        let Some(project) = &self.project else { return };
        let root = project
            .git
            .as_ref()
            .map(|repo| repo.root().to_path_buf())
            .unwrap_or_else(|| project.root.clone());
        if self.github.panel.root.as_ref() != Some(&root) || self.github.panel.repository.is_empty()
        {
            self.github.panel.root = Some(root);
            if let Some(repo) = &project.git {
                let remotes = repo.remotes_with_urls().unwrap_or_default();
                if let Some(repository) = remotes
                    .iter()
                    .find_map(|(name, url)| {
                        (name == "origin")
                            .then(|| server.remote_repository(url))
                            .flatten()
                    })
                    .or_else(|| {
                        remotes
                            .iter()
                            .find_map(|(_, url)| server.remote_repository(url))
                    })
                {
                    self.github.panel.repository = repository;
                }
            }
        }
    }
    fn github_task(
        &mut self,
        label: &str,
        operation: Option<Operation>,
        work: impl FnOnce(Account, String, &rustpy_core::CancellationToken) -> Result<Data, String>
            + Send
            + 'static,
    ) {
        if !self.builtin_plugin_enabled("github") {
            return;
        }

        let Some(account) = self.settings.github.active().cloned() else {
            self.github.panel.error =
                Some("Add an account in Settings → Version Control → GitHub".into());
            return;
        };
        let repository = self.github.panel.repository.clone();
        let root = self.github.panel.root.clone();
        let captured = account.clone();
        let repo = repository.clone();
        let task = TaskManager::global()
            .task(label)
            .spawn(move |progress| work(captured, repo, progress.token()).map_err(task_error));
        self.github.latest.insert(label.into(), task.id());
        self.github.pending.push(Pending {
            label: label.into(),
            task,
            account: account.id,
            repository,
            root,
            operation,
            open_browser: false,
        });
        self.github.panel.status = label.into();
        self.github.panel.error = None;
    }
    pub(super) fn handle_github_event(&mut self, event: Event) {
        if !self.builtin_plugin_enabled("github") {
            self.set_status("GitHub / GitLab bundled capability is disabled in Plugins");
            return;
        }
        match event{
 Event::Accounts=>self.open_settings_page("github"),
 Event::SelectAccount(id)=>{if self.github.busy(){self.set_status("Wait for the current account request or cancel it before switching accounts");return;}self.change_setting_now(|settings|settings.github.active_account=Some(id.clone()));self.github.panel.repository.clear();self.github.panel.root=None;self.github.panel.details=None;self.github.panel.pulls.clear();self.infer_github_repository();self.github_task("Activating GitHub account",None,|account,_,_|{apply_git_account(&account)?;Ok(Data::Activated)});},
 Event::RemoveAccount(id)=>{let Some(account)=self.settings.github.accounts.iter().find(|account|account.id==id).cloned()else{return};let task=TaskManager::global().task("Removing GitHub account").spawn(move|_|{rustpy_secrets::delete(&account.secret_key()).map_err(task_error)?;rustpy_vcs::policy::github_authorization_for(&account.server,String::new()).map_err(task_error)?;Ok(Data::Removed(account.id))});self.github.pending.push(Pending{label:String::new(),task,account:id,repository:String::new(),root:None,operation:None,open_browser:false});},
 Event::Login{server,provider,token,remember,device}=>{
 let server=match Server::new(&server,provider){Ok(server)=>server,Err(error)=>{self.github.panel.error=Some(error);return}};
 let client_id=self.settings.github.oauth_client_id.clone();let(tx,rx)=std::sync::mpsc::channel();self.github.device=Some(rx);
 let task=TaskManager::global().task("Signing in to code hosting").spawn(move|progress|{
 let token=if device{github::auth::authorize(&server,&client_id,progress.token(),|code|{let _=tx.send(code);}).map_err(task_error)?}else{token.trim().to_owned()};
 let client=Client::new(server.clone(),token.clone(),HttpTransport);let response=client.send(&Request::read("/user"),progress.token()).map_err(task_error)?;let user:github::User=serde_json::from_value(response.value).map_err(task_error)?;if user.id==0||user.login.is_empty(){return Err(task_error("The server did not return an account identity"));}
 let account=Account{id:format!("{:?}:{}:{}",provider,server.web,user.id),server:server.web,login:user.login,name:user.name.unwrap_or_default(),scopes:response.scopes,provider};if remember{rustpy_secrets::set(&account.secret_key(),&token)}else{rustpy_secrets::set_for_session(&account.secret_key(),&token)}.map_err(task_error)?;apply_git_account(&account).map_err(task_error)?;Ok(Data::Login(account))
 });self.github.pending.push(Pending{label:String::new(),task,account:String::new(),repository:String::new(),root:None,operation:None,open_browser:false});self.github.panel.error=None;
 },
 Event::Repositories=>{let organization=self.github.panel.organization.clone();let search=self.github.panel.repository_search.clone();self.github_task("Loading repositories",None,move|account,_,cancel|{apply_git_account(&account)?;Client::account(&account)?.repositories(&organization,&search,cancel).map(Data::Repositories)});},
 Event::Organizations=>self.github_task("Loading organizations",None,|account,_,cancel|{let client=Client::account(&account)?;let path=if account.provider==Provider::GitLab{"/groups?membership=true"}else{"/user/orgs"};let values=client.pages(path,None,cancel)?;Ok(Data::Organizations(values.into_iter().filter_map(|value|value[if account.provider==Provider::GitLab{"full_path"}else{"login"}].as_str().map(str::to_owned)).collect()))}),
 Event::Pulls=>{let search=self.github.panel.search.clone();self.github_task("Loading pull requests",None,move|account,repository,cancel|Client::account(&account)?.pulls(&repository,&search,cancel).map(Data::Pulls));},
 Event::Details(number)=>{self.github_task("Loading pull request details",None,move|account,repository,cancel|Client::account(&account)?.details(&repository,number,cancel).map(|details|Data::Details(Box::new(details))));},
 Event::Notifications=>self.github_task("Loading code-hosting notifications",None,|account,_,cancel|{let client=Client::account(&account)?;if account.provider==Provider::GitLab{let values=client.pages("/todos?state=pending",None,cancel)?;Ok(Data::Notifications(values.into_iter().map(|value|github::Notification{id:value["id"].to_string(),unread:true,reason:value["action_name"].as_str().unwrap_or_default().into(),subject:json!({"title":value["body"],"url":value["target_url"],"html_url":value["target_url"]}),repository:github::Repository{full_name:value["project"]["path_with_namespace"].as_str().unwrap_or_default().into(),html_url:value["project"]["web_url"].as_str().unwrap_or_default().into(),..Default::default()},..Default::default()}).collect()))}else{client.pages("/notifications?all=false",None,cancel)?.into_iter().map(|value|serde_json::from_value(value).map_err(|error|error.to_string())).collect::<Result<Vec<_>,_>>().map(Data::Notifications)}}),
 Event::Cancel=>self.github.cancel(),
 Event::Clone(url)=>{let Some(account)=self.settings.github.active()else{return};let Ok(server)=Server::new(&account.server,account.provider)else{return};if server.remote_repository(&url).is_none(){self.github.panel.error=Some("The selected clone URL is not on the account's server".into());return;}let directory=self.github.clone_directory.take().unwrap_or_else(||self.project_creation_directory());let mut state=CloneState::new(directory);state.url=url;self.github.panel.composer=None;self.dialog=Dialog::CloneRepo(Box::new(state));},
 Event::Checkout(pull)=>self.github_checkout_pull(*pull),
 Event::PushBranch=>self.github_push_branch(),
 Event::Write(operation)=>self.github_write(operation),
 Event::Share{operation,remote,initial,files}=>self.github_share(operation,remote,initial,files),
 Event::NewPull=>self.github_new_pull(None),Event::NewGist=>self.github_gist(None),Event::NewShare=>self.github_share_composer(),
 Event::OpenUrl(url)=>{if url.starts_with("https://"){self.open_external_url(&url);}},
 Event::Navigate{path,line}=>{if let Some(root)=&self.github.panel.root{let target=root.join(&path);if !std::path::Path::new(&path).is_absolute()&&!path.contains('\\')&&!path.split('/').any(|part|part=="..")&&target.is_file(){self.mark_nav_jump();self.open_file(&target);if let Some(tab)=self.opened_editor_index(&target).and_then(|index|self.tabs.get_mut(index)){tab.state.go_to_line(line,None);}self.focus_editor=true;}}},
 Event::Plugins(provider)=>self.github_find_plugin(provider),
 }
    }
    fn open_external_url(&mut self, url: &str) {
        self.github.panel.status = format!("Open: {url}");
        self.github.open_urls.push(url.into());
    }
    pub(super) fn github_new_pull(&mut self, branch: Option<String>) {
        self.infer_github_repository();
        let mut composer = Composer::new(ComposeKind::Pull);
        composer.draft = self.settings.github.default_draft;
        composer.body = self.settings.github.pull_request_template.clone();
        if let Some(project) = &self.project {
            composer.head = branch
                .or_else(|| project.git.as_ref().and_then(GitRepo::current_branch))
                .unwrap_or_default();
            self.github.panel.templates = github_templates(&project.root);
            if composer.body.is_empty() {
                if let Some((_, text)) = self.github.panel.templates.first() {
                    composer.body = text.clone();
                }
            }
        }
        self.github.panel.composer = Some(composer);
        self.bind_github_composer();
        self.dialog = Dialog::GitHubModal;
        if self.settings.github.active().is_some() {
            self.github_task(
                "Loading pull request base branch",
                None,
                |account, repository, cancel| {
                    let client = Client::account(&account)?;
                    let path = if account.provider == Provider::GitLab {
                        format!("/projects/{}", github::encode(&repository))
                    } else {
                        github::repository_path(&repository)?
                    };
                    let response = client.send(&Request::read(path), cancel)?;
                    Ok(Data::DefaultBranch(
                        response.value["default_branch"]
                            .as_str()
                            .unwrap_or("main")
                            .into(),
                    ))
                },
            );
        }
    }
    pub(super) fn github_gist(&mut self, path: Option<PathBuf>) {
        let (path, text) = if let Some(path) = path {
            let text = self
                .opened_editor_index(&path)
                .map(|index| self.tabs[index].state.doc.text())
                .or_else(|| std::fs::read_to_string(&path).ok());
            let Some(text) = text else {
                self.set_status("Choose a text file for the gist");
                return;
            };
            (path, text)
        } else {
            let Some(tab) = self.active.and_then(|index| self.tabs.get(index)) else {
                return;
            };
            let text = if tab.state.has_selection() {
                tab.state.doc.slice(tab.state.primary().sel.range())
            } else {
                tab.state.doc.text()
            };
            (
                tab.state
                    .path
                    .clone()
                    .unwrap_or_else(|| PathBuf::from("selection.txt")),
                text,
            )
        };
        if text.len() > 1024 * 1024 {
            self.set_status("Gist input exceeds 1 MiB");
            return;
        }
        let mut composer = Composer::new(ComposeKind::Gist);
        composer.paths.push(path.clone());
        composer.open_browser = true;
        composer.files.insert(
            path.file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
            text,
        );
        self.github.panel.composer = Some(composer);
        self.bind_github_composer();
        self.dialog = Dialog::GitHubModal;
    }
    fn github_write(&mut self, operation: Operation) {
        if !self.builtin_plugin_enabled("github") {
            return;
        }
        if !self.github_composer_current() {
            self.github.panel.error =
                Some("Account or repository changed; reopen the composer".into());
            return;
        }
        if self.safe_mode {
            self.set_status("Trust the project before publishing code or reviews");
            return;
        }
        let anonymous = self
            .github
            .panel
            .composer
            .as_ref()
            .is_some_and(|composer| composer.kind == ComposeKind::Gist && composer.anonymous);
        let open_browser = self
            .github
            .panel
            .composer
            .as_ref()
            .is_some_and(|composer| composer.open_browser);
        let captured = operation.clone();
        if anonymous {
            let task =
                TaskManager::global()
                    .task("Creating anonymous gist")
                    .spawn(move |progress| {
                        let server = Server::new("https://github.com", Provider::GitHub)
                            .map_err(task_error)?;
                        Client::new(server, String::new(), HttpTransport)
                            .execute("", &captured, progress.token())
                            .map(Data::Written)
                            .map_err(task_error)
                    });
            self.github.pending.push(Pending {
                label: String::new(),
                task,
                account: String::new(),
                repository: self.github.panel.repository.clone(),
                root: self.github.panel.root.clone(),
                operation: Some(operation),
                open_browser,
            });
        } else {
            self.github_task(
                "Submitting GitHub action",
                Some(operation),
                move |account, repository, cancel| {
                    Client::account(&account)?
                        .execute(&repository, &captured, cancel)
                        .map(Data::Written)
                },
            );
            if let Some(pending) = self.github.pending.last_mut() {
                pending.open_browser = open_browser;
            }
        }
    }
    fn github_share_composer(&mut self) {
        let Some(project) = &self.project else { return };
        if project
            .git
            .as_ref()
            .is_some_and(|repo| repo.root() != project.root)
        {
            self.set_status("Open the repository root before sharing its history");
            return;
        }
        let mut form = Composer::new(ComposeKind::Share);
        form.title = project
            .root
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        let excluded = project.tree.model().excluded.clone();
        form.paths = ignore::WalkBuilder::new(&project.root)
            .hidden(false)
            .require_git(false)
            .build()
            .filter_map(Result::ok)
            .filter(|entry| {
                entry.file_type().is_some_and(|kind| kind.is_file())
                    && !entry
                        .path()
                        .components()
                        .any(|part| part.as_os_str() == ".git")
                    && !excluded
                        .iter()
                        .any(|excluded| entry.path().starts_with(excluded))
            })
            .take(10000)
            .map(|entry| entry.path().to_path_buf())
            .collect();
        form.checked = form.paths.iter().cloned().collect();
        self.github.panel.root = Some(project.root.clone());
        self.github.panel.composer = Some(form);
        self.bind_github_composer();
        self.dialog = Dialog::GitHubModal;
    }
    fn github_share(
        &mut self,
        operation: Operation,
        remote: String,
        initial: bool,
        files: Vec<PathBuf>,
    ) {
        if !self.github_composer_current()
            || self
                .github
                .panel
                .composer
                .as_ref()
                .and_then(|form| form.root.as_ref())
                != self.project.as_ref().map(|project| &project.root)
        {
            self.github.panel.error =
                Some("Project or account changed; reopen Share Project".into());
            return;
        }
        if self.safe_mode || !self.save_all() {
            return;
        }
        if !rustpy_vcs::is_valid_remote_name(&remote) {
            self.github.panel.error = Some("Choose a valid remote name".into());
            return;
        }
        let Some(root) = self.project.as_ref().map(|project| project.root.clone()) else {
            return;
        };
        let captured = operation.clone();
        self.github_task(
            "Sharing project",
            Some(operation),
            move |account, _, cancel| {
                apply_git_account(&account)?;
                let reservation =
                    rustpy_vcs::RepositoryReservation::try_acquire(&root, "Share project")
                        .map_err(|error| error.to_string())?;
                reservation.run(|| {
                    let value = Client::account(&account)?.execute("", &captured, cancel)?;
                    let repository: github::Repository =
                        serde_json::from_value(value).map_err(|error| error.to_string())?;
                    let publish = (|| -> Result<Option<String>, String> {
                        cancel.check().map_err(|error| error.to_string())?;
                        let server = Server::new(&account.server, account.provider)?;
                        if server.remote_repository(&repository.clone_url).as_deref()
                            != Some(repository.full_name.as_str())
                        {
                            return Err(
                                "Created repository clone URL is outside the selected server"
                                    .into(),
                            );
                        }
                        let repo = GitRepo::discover(&root)
                            .or_else(|| GitRepo::init(&root).ok())
                            .ok_or("Cannot initialize Git repository")?;
                        if repo.root() != root {
                            return Err("The project is inside another Git repository".into());
                        }
                        if let Some((_, url)) = repo
                            .remotes_with_urls()
                            .map_err(|error| error.to_string())?
                            .into_iter()
                            .find(|(name, _)| name == &remote)
                        {
                            if url != repository.clone_url {
                                return Err(
                                    "Remote name already exists with a different URL".into()
                                );
                            }
                        } else {
                            repo.add_remote(&remote, &repository.clone_url)
                                .map_err(|error| error.to_string())?;
                        }
                        if repo.log(1).map_err(|error| error.to_string())?.is_empty() && initial {
                            if files.is_empty() {
                                return Err("No files were selected for the initial commit".into());
                            }
                            if files.iter().any(|path| !path.starts_with(&root)) {
                                return Err("An initial-commit path is outside the project".into());
                            }
                            repo.commit(&files, "Initial commit", false)
                                .map_err(|error| error.to_string())?;
                        }
                        Ok(repo
                            .current_branch()
                            .filter(|branch| !branch.starts_with("HEAD detached")))
                    })();
                    Ok(match publish {
                        Ok(branch) => Data::Shared {
                            repository: Box::new(repository),
                            branch,
                            error: None,
                            remote: remote.clone(),
                        },
                        Err(error) => Data::Shared {
                            repository: Box::new(repository),
                            branch: None,
                            error: Some(error),
                            remote: remote.clone(),
                        },
                    })
                })
            },
        );
    }
    fn github_push_branch(&mut self) {
        if !self.builtin_plugin_enabled("github") {
            return;
        }
        if self.safe_mode || !self.github_composer_current() || !self.save_all() {
            return;
        }
        let Some(form) = self.github.panel.composer.as_ref() else {
            return;
        };
        let Some(root) = form.root.clone() else {
            return;
        };
        let Some(repo) = GitRepo::discover(&root) else {
            return;
        };
        let branch = form.head.clone();
        if repo.current_branch().as_deref() != Some(branch.as_str()) {
            self.set_status("Select the current local branch as the PR head before pushing");
            return;
        }
        let Some(account) = self.settings.github.active() else {
            return;
        };
        let Ok(server) = Server::new(&account.server, account.provider) else {
            return;
        };
        let Some(remote) = repo
            .remotes_with_urls()
            .unwrap_or_default()
            .into_iter()
            .find(|(_, url)| {
                server.remote_repository(url).as_deref() == Some(form.repository.as_str())
            })
            .map(|(name, _)| name)
        else {
            self.set_status("No local remote matches the selected pull request repository");
            return;
        };
        self.github_task(
            "Preparing pull request branch push",
            None,
            move |account, _, _| {
                apply_git_account(&account)?;
                Ok(Data::Push {
                    root,
                    remote,
                    branch,
                })
            },
        );
    }
    fn github_checkout_pull(&mut self, pull: github::PullRequest) {
        if !self.builtin_plugin_enabled("github") {
            return;
        }
        if self.safe_mode || !self.save_all() {
            return;
        }
        let Some(root) = self.github.panel.root.clone() else {
            return;
        };
        let provider = self
            .settings
            .github
            .active()
            .map(|account| account.provider)
            .unwrap_or_default();
        let Some(repo) = GitRepo::discover(&root) else {
            return;
        };
        let Some(account) = self.settings.github.active() else {
            return;
        };
        let Ok(server) = Server::new(&account.server, account.provider) else {
            return;
        };
        let Some(remote) = repo
            .remotes_with_urls()
            .ok()
            .and_then(|remotes| {
                remotes.into_iter().find(|(_, url)| {
                    server.remote_repository(url).as_deref()
                        == Some(self.github.panel.repository.as_str())
                })
            })
            .map(|(name, _)| name)
        else {
            self.set_status("Select a mapped repository with a remote matching this pull request");
            return;
        };
        let branch = format!(
            "{}-{}",
            if provider == Provider::GitLab {
                "mr"
            } else {
                "pr"
            },
            pull.number
        );
        if repo
            .branches()
            .iter()
            .any(|entry| !entry.is_remote && entry.name == branch)
        {
            self.set_status(format!(
                "Branch {branch} already exists; check it out from Branches"
            ));
            return;
        }
        let source = if provider == Provider::GitLab {
            format!("refs/merge-requests/{}/head", pull.number)
        } else {
            format!("refs/pull/{}/head", pull.number)
        };
        let reference = format!(
            "refs/rustpy/pull-requests/{}-{}",
            pull.number, self.next_session_id
        );
        self.next_session_id += 1;
        self.github.checkout_after_fetch = Some((
            root.clone(),
            branch.clone(),
            pull.head.sha,
            reference.clone(),
        ));
        self.start_git_op(crate::git_jobs::GitOp::BranchNetwork {
            root,
            label: "Fetch pull request branch".into(),
            args: vec![
                "fetch".into(),
                "--progress".into(),
                "--".into(),
                remote,
                format!("{source}:{reference}"),
            ],
        });
    }
    pub(super) fn poll_github(&mut self, ctx: &egui::Context) {
        if !self.builtin_plugin_enabled("github") {
            self.github.cancel();
            self.github.open_urls.clear();
            self.github.panel.composer = None;
            self.github.device = None;
        }
        for url in std::mem::take(&mut self.github.open_urls) {
            ctx.open_url(egui::OpenUrl::new_tab(url));
        }
        if let Some(receiver) = &self.github.device {
            for code in receiver.try_iter() {
                self.github.panel.device_code = Some(code);
            }
        }
        let mut results = Vec::new();
        for (index, pending) in self.github.pending.iter_mut().enumerate() {
            if let Some(result) = pending.task.try_result() {
                results.push((index, result));
            }
        }
        for (index, result) in results.into_iter().rev() {
            let pending = self.github.pending.remove(index);
            let current_request = pending.label.is_empty()
                || self.github.latest.get(&pending.label) == Some(&pending.task.id());
            let same = self.builtin_plugin_enabled("github")
                && current_request
                && (pending.account.is_empty()
                    || self
                        .settings
                        .github
                        .active()
                        .is_some_and(|account| account.id == pending.account))
                && (pending.repository.is_empty()
                    || pending.repository == self.github.panel.repository);
            match result {
                Ok(data) => self.github_completed(data, pending, same, ctx),
                Err(error) => {
                    if same {
                        self.github.panel.error = Some(error.to_string());
                    }
                    self.notify_error("GitHub", "Code hosting request failed", error.to_string());
                }
            }
        }
        if self.github.busy() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        if self.bottom == Some(BottomTool::PullRequests)
            && !self.coordinated_quit_active()
            && self.settings.github.active().is_some()
            && !self.github.panel.repository.is_empty()
        {
            let now = ctx.input(|input| input.time);
            let interval = self.settings.github.refresh_seconds.clamp(30, 3600) as f64;
            if !self.github.busy() && now - self.github.panel.last_refresh >= interval {
                self.github.panel.last_refresh = now;
                self.handle_github_event(Event::Pulls);
                if self.settings.github.notify {
                    self.handle_github_event(Event::Notifications);
                }
            }
            ctx.request_repaint_after(std::time::Duration::from_secs(
                self.settings.github.refresh_seconds.clamp(30, 3600),
            ));
        }
        self.sync_github_review_gutter();
    }
    fn github_completed(&mut self, data: Data, pending: Pending, same: bool, ctx: &egui::Context) {
        match data {
            Data::Login(account) => {
                let id = account.id.clone();
                self.change_setting_now(|settings| {
                    settings
                        .github
                        .accounts
                        .retain(|existing| existing.id != id);
                    settings.github.accounts.push(account.clone());
                    settings.github.active_account = Some(id.clone());
                });
                self.github.panel.device_code = None;
                self.github.panel.status = "Signed in".into();
            }
            Data::Activated => {}
            Data::DefaultBranch(branch) if same => {
                if let Some(form) = self
                    .github
                    .panel
                    .composer
                    .as_mut()
                    .filter(|form| form.kind == ComposeKind::Pull && form.base == "main")
                {
                    form.base = branch;
                }
            }
            Data::Push {
                root,
                remote,
                branch,
            } if same
                && !self.safe_mode
                && !self.coordinated_quit_active()
                && self.github.panel.root.as_ref() == Some(&root) =>
            {
                self.start_git_op(crate::git_jobs::GitOp::BranchNetwork {
                    root,
                    label: "Push pull request head branch".into(),
                    args: vec![
                        "push".into(),
                        "--set-upstream".into(),
                        remote,
                        format!("refs/heads/{branch}:refs/heads/{branch}"),
                    ],
                });
            }
            Data::Removed(id) => {
                self.github.panel.details = None;
                self.github.panel.pending_comments.clear();
                self.github.panel.pending_review = None;
                self.change_setting_now(|settings| {
                    settings.github.accounts.retain(|account| account.id != id);
                    if settings.github.active_account.as_ref() == Some(&id) {
                        settings.github.active_account = settings
                            .github
                            .accounts
                            .first()
                            .map(|account| account.id.clone());
                    }
                });
                if self.settings.github.active().is_some() {
                    self.github_task("Activating GitHub account", None, |account, _, _| {
                        apply_git_account(&account)?;
                        Ok(Data::Activated)
                    });
                }
            }
            Data::Repositories(repositories) if same => {
                self.github.panel.repositories = repositories
            }
            Data::Organizations(organizations) if same => {
                self.github.panel.organizations = organizations
            }
            Data::Pulls(pulls) if same => {
                self.github.panel.pulls = pulls;
                self.github.panel.last_refresh = ctx.input(|input| input.time);
            }
            Data::Details(details) if same => {
                if self.github.panel.details_account != pending.account
                    || self.github.panel.details_repository != pending.repository
                    || self
                        .github
                        .panel
                        .details
                        .as_ref()
                        .map(|previous| previous.pull.number)
                        != Some(details.pull.number)
                {
                    self.github.panel.pending_comments.clear();
                    self.github.panel.pending_review = None;
                }
                let login = self
                    .settings
                    .github
                    .accounts
                    .iter()
                    .find(|account| account.id == pending.account)
                    .map(|account| account.login.as_str())
                    .unwrap_or_default();
                self.github.panel.details_account = pending.account.clone();
                self.github.panel.details_repository = pending.repository.clone();
                self.github.panel.selected_file = 0;
                self.github.panel.pending_review = details
                    .reviews
                    .iter()
                    .find(|review| review.state == "PENDING" && review.user.login == login)
                    .map(|review| review.id);
                self.github.panel.details = Some(*details);
            }
            Data::Notifications(notifications) if same => {
                let newest = notifications
                    .first()
                    .map(|notification| notification.id.clone());
                if self.settings.github.notify
                    && self.github.panel.notification_cursor.is_some()
                    && newest != self.github.panel.notification_cursor
                {
                    self.notify(
                        crate::notifications::NotificationKind::Info,
                        "GitHub",
                        "New code review notifications",
                        format!("{} unread notifications", notifications.len()),
                        vec![(
                            "Pull Requests".into(),
                            crate::notifications::NotificationAction::RunAction(
                                Action::GitHubPullRequests,
                            ),
                        )],
                    );
                }
                self.github.panel.notification_cursor = newest;
                self.github.panel.notifications = notifications;
            }
            Data::Written(value) => {
                let url = value["html_url"]
                    .as_str()
                    .or_else(|| value["web_url"].as_str())
                    .map(str::to_owned);
                if pending
                    .operation
                    .as_ref()
                    .is_some_and(|operation| matches!(operation, Operation::CreateGist { .. }))
                {
                    self.github.panel.last_gist_url = url.clone();
                }
                if pending.open_browser {
                    if let Some(url) = &url {
                        ctx.open_url(egui::OpenUrl::new_tab(url));
                    }
                }
                if same && !self.coordinated_quit_active() {
                    self.github.panel.error = None;
                    self.github.panel.pending_comments.clear();
                    if value["state"] == "PENDING" {
                        self.github.panel.pending_review = value["id"].as_u64();
                    } else {
                        self.github.panel.pending_review = None;
                        self.github.panel.pending_comments.clear();
                    }
                    self.github.panel.composer = None;
                    self.dialog = Dialog::None;
                    let number = value["number"]
                        .as_u64()
                        .or_else(|| pending.operation.as_ref().and_then(operation_number));
                    if let Some(number) = number {
                        self.handle_github_event(Event::Details(number));
                    }
                }
                let message = if let Some(warnings) = value.get("rustpy_metadata_warnings") {
                    format!("Created pull request; metadata updates need attention: {warnings}")
                } else {
                    url.clone().unwrap_or_else(|| "Request submitted".into())
                };
                self.notify(
                    crate::notifications::NotificationKind::Info,
                    "GitHub",
                    "Code hosting action completed",
                    message,
                    vec![],
                );
            }
            Data::Shared {
                repository,
                branch,
                error,
                remote,
            } => {
                if let Some(root) = pending.root {
                    if self
                        .project
                        .as_ref()
                        .is_some_and(|project| project.root == root)
                    {
                        self.vcs_workspace.mappings.register(root.clone());
                        let _ = self.vcs_workspace.mappings.save(&root);
                        if let Some(project) = &mut self.project {
                            project.git = GitRepo::discover(&root);
                            project.git_refreshed = -100.0;
                        }
                    }
                    if let Some(error) = error {
                        self.github.panel.error = Some(format!(
                            "Repository created at {}; local publishing needs attention: {error}",
                            repository.html_url
                        ));
                    } else if let Some(branch) = branch {
                        if !same || self.coordinated_quit_active() {
                            self.set_status(format!(
                                "Repository created at {}; select its account to publish",
                                repository.html_url
                            ));
                            return;
                        }
                        self.start_git_op(crate::git_jobs::GitOp::BranchNetwork {
                            root,
                            label: "Publish shared project".into(),
                            args: vec![
                                "push".into(),
                                "-u".into(),
                                "--progress".into(),
                                "--".into(),
                                remote,
                                format!("refs/heads/{branch}:refs/heads/{branch}"),
                            ],
                        });
                        self.github.panel.composer = None;
                        self.dialog = Dialog::None;
                    } else {
                        self.set_status(format!(
                            "Created {}; make a commit before pushing",
                            repository.html_url
                        ));
                    }
                }
            }
            _ => {}
        }
    }
}
fn operation_number(operation: &Operation) -> Option<u64> {
    match operation {
        Operation::Review { number, .. }
        | Operation::SubmitReview { number, .. }
        | Operation::InlineComment { number, .. }
        | Operation::Comment { number, .. }
        | Operation::Merge { number, .. }
        | Operation::Reviewers { number, .. }
        | Operation::Metadata { number, .. } => Some(*number),
        _ => None,
    }
}
fn github_templates(root: &Path) -> Vec<(String, String)> {
    let mut paths = vec![
        root.join(".github/pull_request_template.md"),
        root.join(".github/PULL_REQUEST_TEMPLATE.md"),
        root.join("PULL_REQUEST_TEMPLATE.md"),
        root.join("docs/pull_request_template.md"),
    ];
    if let Ok(entries) = std::fs::read_dir(root.join(".github/PULL_REQUEST_TEMPLATE")) {
        paths.extend(entries.filter_map(Result::ok).map(|entry| entry.path()));
    }
    paths
        .into_iter()
        .filter(|path| std::fs::metadata(path).is_ok_and(|metadata| metadata.len() <= 1024 * 1024))
        .filter_map(|path| {
            std::fs::read_to_string(&path).ok().map(|text| {
                (
                    path.strip_prefix(root)
                        .unwrap_or(&path)
                        .display()
                        .to_string(),
                    text,
                )
            })
        })
        .collect()
}

impl IdeApp {
    pub(super) fn bind_github_composer(&mut self) {
        let Some(form) = &mut self.github.panel.composer else {
            return;
        };
        if form.bound {
            return;
        }
        let details = matches!(
            form.kind,
            ComposeKind::Review | ComposeKind::Inline | ComposeKind::Comment | ComposeKind::Merge
        );
        form.account = if details {
            Some(self.github.panel.details_account.clone())
        } else {
            self.settings
                .github
                .active()
                .map(|account| account.id.clone())
        };
        form.repository = if details {
            self.github.panel.details_repository.clone()
        } else {
            self.github.panel.repository.clone()
        };
        form.root = self.github.panel.root.clone();
        form.bound = true;
    }
    fn github_composer_current(&self) -> bool {
        self.github.panel.composer.as_ref().is_some_and(|form| {
            form.bound
                && form.repository == self.github.panel.repository
                && form.root == self.github.panel.root
                && ((form.kind == ComposeKind::Gist && form.anonymous)
                    || form.account.as_ref()
                        == self.settings.github.active().map(|account| &account.id))
        })
    }
    pub(super) fn github_git_job_finished(&mut self, done: &crate::git_jobs::FinishedJob) {
        let crate::git_jobs::GitOp::BranchNetwork { root, label, .. } = &done.op else {
            return;
        };
        if label != "Fetch pull request branch" {
            return;
        }
        let Some((expected, branch, sha, reference)) = self.github.checkout_after_fetch.take()
        else {
            return;
        };
        if &expected != root || !done.success {
            return;
        }
        if self.coordinated_quit_active() {
            return;
        }
        let Some(repo) = GitRepo::discover(root) else {
            return;
        };
        if repo.github_revision(&reference).ok().as_deref() != Some(sha.as_str()) {
            self.set_status("Pull request head changed while fetching; refresh before checkout");
            return;
        }
        if self
            .project
            .as_ref()
            .and_then(|project| project.git.as_ref())
            .is_none_or(|repo| repo.root() != root)
        {
            self.set_status(format!(
                "Fetched {branch}; select its repository before checkout"
            ));
            return;
        }
        self.handle_branch_workspace_event(crate::git_panel::branch_workspace::Event {
            root: root.clone(),
            command: crate::git_panel::branch_workspace::Command::Local(
                rustpy_vcs::branches::LocalAction::Create {
                    name: branch,
                    start: reference,
                    checkout: true,
                    overwrite: false,
                },
            ),
        });
    }
    pub(super) fn sync_github_review_gutter(&mut self) {
        let context = self
            .github
            .panel
            .details
            .as_ref()
            .filter(|_| {
                self.github.panel.details_repository == self.github.panel.repository
                    && self
                        .settings
                        .github
                        .active()
                        .is_some_and(|account| account.id == self.github.panel.details_account)
            })
            .and_then(|details| {
                self.github.panel.root.as_ref().map(|root| {
                    (
                        root.clone(),
                        details.pull.head.sha.clone(),
                        details.comments.clone(),
                    )
                })
            });
        let Some((root, sha, comments)) =
            context.filter(|_| self.settings.github.show_review_gutter)
        else {
            self.github.gutter_stamp = None;
            for tab in &mut self.tabs {
                tab.view.review_comments.clear();
            }
            return;
        };
        let Some(repo) = GitRepo::discover(&root) else {
            self.github.gutter_stamp = None;
            for tab in &mut self.tabs {
                tab.view.review_comments.clear();
            }
            return;
        };
        let head = repo.github_revision("HEAD").unwrap_or_default();
        let stamp = format!(
            "{}:{sha}:{head}:{:?}:{:?}",
            root.display(),
            comments
                .iter()
                .map(|comment| (comment.id, comment.line, &comment.body))
                .collect::<Vec<_>>(),
            self.tabs
                .iter()
                .map(|tab| (
                    tab.state.doc.identity(),
                    tab.state.doc.version(),
                    &tab.state.path
                ))
                .collect::<Vec<_>>()
        );
        if self.github.gutter_stamp.as_ref() == Some(&stamp) {
            return;
        }
        self.github.gutter_stamp = Some(stamp);
        for tab in &mut self.tabs {
            tab.view.review_comments.clear();
            if head != sha {
                continue;
            }
            let Some(path) = &tab.state.path else {
                continue;
            };
            let Ok(relative) = path.strip_prefix(&root) else {
                continue;
            };
            let relative = relative.to_string_lossy().replace('\\', "/");
            let applicable: Vec<_> = comments
                .iter()
                .filter(|comment| {
                    comment.path.as_deref() == Some(relative.as_str())
                        && comment.side.as_deref() == Some("RIGHT")
                        && comment.commit_id.as_deref() == Some(sha.as_str())
                        && comment.line.is_some()
                })
                .collect();
            if applicable.is_empty()
                || repo.file_at_commit(&sha, path).ok().as_deref()
                    != Some(tab.state.doc.text().as_str())
            {
                continue;
            }
            for comment in applicable {
                let line = comment.line.unwrap_or(1).saturating_sub(1);
                if line < tab.state.doc.line_count() {
                    tab.view
                        .review_comments
                        .push(crate::editor::review_comments::Marker {
                            offset: tab.state.doc.line_range(line).start(),
                            id: comment.id,
                            text: format!("{}: {}", comment.user.login, comment.body),
                            document: tab.state.doc.identity(),
                            version: tab.state.doc.version(),
                        });
                }
            }
        }
    }
    pub(super) fn open_github_review_comment(&mut self, index: usize, id: u64) {
        let Some(tab) = self.tabs.get(index) else {
            return;
        };
        if !tab.view.review_comments.iter().any(|marker| {
            marker.id == id
                && marker.document == tab.state.doc.identity()
                && marker.version == tab.state.doc.version()
        }) {
            return;
        }
        let Some(details) = &self.github.panel.details else {
            return;
        };
        let Some(comment) = details.comments.iter().find(|comment| comment.id == id) else {
            return;
        };
        self.github.panel.selected_file = details
            .files
            .iter()
            .position(|file| Some(file.filename.as_str()) == comment.path.as_deref())
            .unwrap_or(0);
        self.github.panel.selected_comment = Some(id);
        self.github.panel.tab = 2;
        self.bottom = Some(BottomTool::PullRequests);
    }
    fn github_web_context(&self, root: &std::path::Path) -> Option<(Server, String)> {
        let repo = GitRepo::discover(root)?;
        let mut servers: Vec<Server> = self
            .settings
            .github
            .accounts
            .iter()
            .filter_map(|account| Server::new(&account.server, account.provider).ok())
            .collect();
        servers.push(Server::new("https://github.com", Provider::GitHub).ok()?);
        servers.push(Server::new("https://gitlab.com", Provider::GitLab).ok()?);
        let remotes = repo.remotes_with_urls().ok()?;
        for name in ["origin", "upstream"] {
            for (_, url) in remotes.iter().filter(|(remote, _)| remote == name) {
                for server in &servers {
                    if let Some(repository) = server.remote_repository(url) {
                        return Some((server.clone(), repository));
                    }
                }
            }
        }
        for (_, url) in remotes {
            for server in &servers {
                if let Some(repository) = server.remote_repository(&url) {
                    return Some((server.clone(), repository));
                }
            }
        }
        None
    }
    pub(super) fn github_open_file(&mut self) {
        let Some(tab) = self.active.and_then(|index| self.tabs.get(index)) else {
            return;
        };
        let Some(path) = &tab.state.path else { return };
        let Some(repo) = GitRepo::discover(path.parent().unwrap_or(path)) else {
            return;
        };
        let root = repo.root();
        let Some((server, repository)) = self.github_web_context(root) else {
            return;
        };
        let Ok(relative) = path.strip_prefix(root) else {
            return;
        };
        let Ok(revision) = repo.github_revision("HEAD") else {
            return;
        };
        let selection = tab.state.primary().sel.range();
        let first = tab.state.doc.line_of(selection.start()) + 1;
        let end = if selection.end() > selection.start() {
            selection.end() - rustpy_core::TextSize::from(1)
        } else {
            selection.end()
        };
        let last = tab.state.doc.line_of(end) + 1;
        let url = server.file_url(
            &repository,
            &revision,
            &relative.to_string_lossy().replace('\\', "/"),
            Some(first),
            Some(last),
        );
        self.open_external_url(&url);
    }
    pub(super) fn github_open_commit(&mut self, root: PathBuf, commit: String) {
        let Some((server, repository)) = self.github_web_context(&root) else {
            return;
        };
        let path = if server.provider == Provider::GitLab {
            "-/commit"
        } else {
            "commit"
        };
        self.open_external_url(&format!(
            "{}/{repository}/{path}/{}",
            server.web,
            github::encode(&commit)
        ));
    }
    pub(super) fn github_open_issue(&mut self) {
        let Some(tab) = self.active.and_then(|index| self.tabs.get(index)) else {
            return;
        };
        let Some(path) = &tab.state.path else { return };
        let Some(repo) = GitRepo::discover(path.parent().unwrap_or(path)) else {
            return;
        };
        let Some((server, repository)) = self.github_web_context(repo.root()) else {
            return;
        };
        let line = tab
            .state
            .doc
            .line_text(tab.state.doc.line_of(tab.state.primary().head()));
        if let Some(url) = crate::github_panel::issue_urls(&server, &repository, &line).first() {
            self.open_external_url(url);
        }
    }
}

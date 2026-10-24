//! Pull requests, account management and explicit outbound action composers.
use crate::a11y::AccessibleCombo;
use crate::a11y::DialogWindow;
use crate::theme::UiTheme;
use rustpy_settings::github::{GitHubSettings, Provider};
use rustpy_vcs::github::*;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
};
#[derive(Clone, Debug)]
pub enum Event {
    Accounts,
    SelectAccount(String),
    Login {
        server: String,
        provider: Provider,
        token: String,
        remember: bool,
        device: bool,
    },
    RemoveAccount(String),
    Repositories,
    Organizations,
    Pulls,
    Details(u64),
    Notifications,
    Cancel,
    Clone(String),
    Checkout(Box<PullRequest>),
    Write(Operation),
    Share {
        operation: Operation,
        remote: String,
        initial: bool,
        files: Vec<PathBuf>,
    },
    NewPull,
    PushBranch,
    NewGist,
    NewShare,
    OpenUrl(String),
    Navigate {
        path: String,
        line: usize,
    },
    Plugins(String),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ComposeKind {
    Gist,
    Share,
    Pull,
    Review,
    Inline,
    Comment,
    Merge,
    Repositories,
}
#[derive(Clone, Debug)]
pub struct Composer {
    pub kind: ComposeKind,
    pub account: Option<String>,
    pub repository: String,
    pub root: Option<PathBuf>,
    pub bound: bool,
    pub title: String,
    pub body: String,
    pub head: String,
    pub base: String,
    pub draft: bool,
    pub reviewers: String,
    pub assignees: String,
    pub labels: String,
    pub private: bool,
    pub anonymous: bool,
    pub remote: String,
    pub organization: String,
    pub initial: bool,
    pub files: BTreeMap<String, String>,
    pub paths: Vec<PathBuf>,
    pub checked: BTreeSet<PathBuf>,
    pub inline: InlineComment,
    pub reply_to: Option<u64>,
    pub review_result: String,
    pub merge_method: String,
    pub sha: String,
    pub number: u64,
    pub review_id: Option<u64>,
    pub open_browser: bool,
}
impl Composer {
    pub fn new(kind: ComposeKind) -> Self {
        Self {
            kind,
            account: None,
            repository: String::new(),
            root: None,
            bound: false,
            title: String::new(),
            body: String::new(),
            head: String::new(),
            base: "main".into(),
            draft: false,
            reviewers: String::new(),
            assignees: String::new(),
            labels: String::new(),
            private: true,
            anonymous: false,
            remote: "origin".into(),
            organization: String::new(),
            initial: true,
            files: BTreeMap::new(),
            paths: Vec::new(),
            checked: BTreeSet::new(),
            inline: InlineComment::default(),
            reply_to: None,
            review_result: "COMMENT".into(),
            merge_method: "merge".into(),
            sha: String::new(),
            number: 0,
            review_id: None,
            open_browser: false,
        }
    }
}
pub struct Workspace {
    pub repository: String,
    pub root: Option<PathBuf>,
    pub search: Search,
    pub repositories: Vec<Repository>,
    pub organizations: Vec<String>,
    pub repository_search: String,
    pub organization: String,
    pub pulls: Vec<PullRequest>,
    pub details: Option<PullDetails>,
    pub details_account: String,
    pub details_repository: String,
    pub notifications: Vec<Notification>,
    pub composer: Option<Composer>,
    pub templates: Vec<(String, String)>,
    pub pending_comments: Vec<InlineComment>,
    pub pending_review: Option<u64>,
    pub selected_file: usize,
    pub selected_comment: Option<u64>,
    pub status: String,
    pub error: Option<String>,
    pub tab: usize,
    pub login_server: String,
    pub login_provider: Provider,
    pub login_token: String,
    pub remember: bool,
    pub device_code: Option<auth::DeviceCode>,
    pub last_gist_url: Option<String>,
    pub last_refresh: f64,
    pub notification_cursor: Option<String>,
    pub show_notifications: bool,
}
impl Default for Workspace {
    fn default() -> Self {
        Self {
            repository: String::new(),
            root: None,
            search: Search {
                state: "open".into(),
                ..Default::default()
            },
            repositories: Vec::new(),
            organizations: Vec::new(),
            repository_search: String::new(),
            organization: String::new(),
            pulls: Vec::new(),
            details: None,
            details_account: String::new(),
            details_repository: String::new(),
            notifications: Vec::new(),
            composer: None,
            templates: Vec::new(),
            pending_comments: Vec::new(),
            pending_review: None,
            selected_file: 0,
            selected_comment: None,
            status: String::new(),
            error: None,
            tab: 0,
            login_server: "https://github.com".into(),
            login_provider: Provider::GitHub,
            login_token: String::new(),
            remember: true,
            device_code: None,
            last_gist_url: None,
            last_refresh: 0.0,
            notification_cursor: None,
            show_notifications: false,
        }
    }
}
fn names(text: &str) -> Vec<String> {
    text.split(',')
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
        .collect()
}
impl Workspace {
    pub fn account_ui(
        &mut self,
        ui: &mut egui::Ui,
        settings: &mut GitHubSettings,
        busy: bool,
    ) -> Vec<Event> {
        let mut events = Vec::new();
        ui.heading("GitHub accounts");
        for account in &settings.accounts {
            ui.horizontal(|ui| {
                if ui
                    .selectable_label(
                        settings.active_account.as_ref() == Some(&account.id),
                        format!("{} — {}", account.login, account.server),
                    )
                    .clicked()
                {
                    events.push(Event::SelectAccount(account.id.clone()));
                }
                ui.label(account.scopes.join(", "));
                if ui.add_enabled(!busy, egui::Button::new("Remove")).clicked() {
                    events.push(Event::RemoveAccount(account.id.clone()));
                }
            });
        }
        ui.separator();
        ui.horizontal(|ui| {
            ui.label("Server");
            ui.text_edit_singleline(&mut self.login_server);
        });
        egui::ComboBox::from_label("Provider")
            .selected_text(format!("{:?}", self.login_provider))
            .show_ui(ui, |ui| {
                ui.selectable_value(
                    &mut self.login_provider,
                    Provider::GitHub,
                    "GitHub / GitHub Enterprise",
                );
                ui.selectable_value(&mut self.login_provider, Provider::GitLab, "GitLab adapter");
            });
        ui.horizontal(|ui| {
            ui.label("Access token");
            ui.add(egui::TextEdit::singleline(&mut self.login_token).password(true));
        });
        ui.checkbox(
            &mut self.remember,
            "Store token in the configured password safe",
        );
        ui.horizontal(|ui| {
            if ui
                .add_enabled(
                    !busy && !self.login_token.trim().is_empty(),
                    egui::Button::new("Log in with Token"),
                )
                .clicked()
            {
                events.push(Event::Login {
                    server: self.login_server.clone(),
                    provider: self.login_provider,
                    token: std::mem::take(&mut self.login_token),
                    remember: self.remember,
                    device: false,
                });
            }
            if ui
                .add_enabled(
                    !busy && self.login_provider == Provider::GitHub,
                    egui::Button::new("Log in via OAuth Device Flow"),
                )
                .clicked()
            {
                events.push(Event::Login {
                    server: self.login_server.clone(),
                    provider: self.login_provider,
                    token: String::new(),
                    remember: self.remember,
                    device: true,
                });
            }
        });
        ui.horizontal(|ui| {
            ui.label("OAuth application client ID");
            ui.text_edit_singleline(&mut settings.oauth_client_id);
        });
        if let Some(code) = &self.device_code {
            ui.label(format!(
                "Enter {} to authorize this device (expires in {} seconds)",
                code.code, code.expires_in
            ));
            ui.hyperlink_to("Open device authorization", &code.uri);
        }
        ui.checkbox(
            &mut settings.notify,
            "Notify about pull requests and reviews",
        );
        ui.checkbox(
            &mut settings.show_review_gutter,
            "Show code review comments in the editor gutter",
        );
        ui.checkbox(&mut settings.show_actions, "Show GitHub Actions status");
        ui.checkbox(
            &mut settings.default_draft,
            "Create draft pull requests by default",
        );
        ui.horizontal(|ui| {
            ui.label("Refresh interval");
            ui.add(
                egui::DragValue::new(&mut settings.refresh_seconds)
                    .range(30..=3600)
                    .suffix(" s"),
            );
        });
        ui.label("Fallback pull request description template");
        ui.text_edit_multiline(&mut settings.pull_request_template);
        ui.horizontal_wrapped(|ui| {
            for provider in ["GitLab", "Bitbucket", "Gerrit", "Azure DevOps"] {
                if ui.link(format!("{provider} plugins")).clicked() {
                    events.push(Event::Plugins(provider.into()));
                }
            }
        });
        if busy {
            crate::a11y::spinner(ui);
            if ui.button("Cancel sign-in / request").clicked() {
                events.push(Event::Cancel);
            }
        }
        if let Some(error) = &self.error {
            ui.colored_label(ui.visuals().error_fg_color, error);
        }
        events
    }
    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        theme: &UiTheme,
        settings: &GitHubSettings,
        busy: bool,
    ) -> Vec<Event> {
        let mut events = Vec::new();
        ui.horizontal_wrapped(|ui| {
            if ui.button("Accounts…").clicked() {
                events.push(Event::Accounts);
            }
            egui::ComboBox::from_id_salt("github-account")
                .selected_text(
                    settings
                        .active()
                        .map(|account| format!("{} @ {}", account.login, account.server))
                        .unwrap_or_else(|| "Sign in".into()),
                )
                .show_ui(ui, |ui| {
                    for account in &settings.accounts {
                        if ui
                            .selectable_label(
                                settings.active_account.as_ref() == Some(&account.id),
                                &account.login,
                            )
                            .clicked()
                        {
                            events.push(Event::SelectAccount(account.id.clone()));
                        }
                    }
                })
                .combo_named(crate::i18n::tr_static("GitHub account"));
            ui.label("Repository");
            ui.add(
                egui::TextEdit::singleline(&mut self.repository)
                    .desired_width(180.0)
                    .hint_text("owner/repository"),
            );
            if ui.button("Load / Refresh").clicked() {
                events.push(Event::Pulls);
            }
            if ui.button("Repositories…").clicked() {
                self.composer = Some(Composer::new(ComposeKind::Repositories));
                events.push(Event::Repositories);
            }
            if ui.button("Create Gist…").clicked() {
                events.push(Event::NewGist);
            }
            if let Some(url) = &self.last_gist_url {
                ui.hyperlink_to("Last created Gist", url);
            }
            if ui.button("Create PR…").clicked() {
                events.push(Event::NewPull);
            }
            if ui.button("Notifications").clicked() {
                self.show_notifications = !self.show_notifications;
                events.push(Event::Notifications);
            }
        });
        if settings.accounts.is_empty() {
            ui.label("Add an account in Settings → Version Control → GitHub.");
            return events;
        }
        ui.horizontal_wrapped(|ui| {
            egui::ComboBox::from_label("State")
                .selected_text(&self.search.state)
                .show_ui(ui, |ui| {
                    for state in ["open", "closed", "merged", "all"] {
                        ui.selectable_value(
                            &mut self.search.state,
                            state.into(),
                            crate::i18n::tr_static(state),
                        );
                    }
                });
            for (label, value) in [
                ("Author", &mut self.search.author),
                ("Assignee", &mut self.search.assignee),
                ("Review", &mut self.search.review),
                ("Label", &mut self.search.label),
            ] {
                ui.label(crate::i18n::tr_static(label));
                ui.add(egui::TextEdit::singleline(value).desired_width(85.0));
            }
            ui.label("Search syntax");
            ui.add(
                egui::TextEdit::singleline(&mut self.search.text)
                    .desired_width(200.0)
                    .hint_text("is:draft review-requested:@me updated:>=2026-01-01"),
            );
            if ui.button("Search").clicked() {
                events.push(Event::Pulls);
            }
        });
        if busy {
            ui.horizontal(|ui| {
                crate::a11y::spinner(ui);
                ui.label(&self.status);
                if ui.button("Cancel").clicked() {
                    events.push(Event::Cancel);
                }
            });
        }
        if let Some(error) = &self.error {
            ui.colored_label(theme.error, error);
        }
        if self.show_notifications {
            egui::CollapsingHeader::new("GitHub notifications")
                .default_open(true)
                .show(ui, |ui| {
                    for notification in &self.notifications {
                        ui.horizontal(|ui| {
                            ui.label(&notification.reason);
                            if let Some(title) = notification.subject["title"].as_str() {
                                if ui
                                    .link(format!("{}: {title}", notification.repository.full_name))
                                    .clicked()
                                {
                                    self.repository = notification.repository.full_name.clone();
                                    if let Some(number) = notification.subject["url"]
                                        .as_str()
                                        .and_then(|url| url.rsplit('/').next())
                                        .and_then(|number| number.parse().ok())
                                    {
                                        if notification.subject["type"] == "Issue" {
                                            let url = format!(
                                                "{}/issues/{number}",
                                                notification.repository.html_url
                                            );
                                            events.push(Event::OpenUrl(url));
                                        } else if let Some(url) =
                                            notification.subject["html_url"].as_str()
                                        {
                                            events.push(Event::OpenUrl(url.into()));
                                        } else {
                                            events.push(Event::Details(number));
                                        }
                                    }
                                }
                            }
                        });
                    }
                });
        }
        egui::ScrollArea::vertical()
            .id_salt("github-pull-list")
            .max_height(180.0)
            .show(ui, |ui| {
                for pull in &self.pulls {
                    let selected = self
                        .details
                        .as_ref()
                        .is_some_and(|details| details.pull.number == pull.number);
                    if ui
                        .selectable_label(
                            selected,
                            format!(
                                "#{} {}{} — {}",
                                pull.number,
                                if pull.draft { "Draft: " } else { "" },
                                pull.title,
                                pull.user.login
                            ),
                        )
                        .clicked()
                    {
                        events.push(Event::Details(pull.number));
                    }
                }
            });
        if let Some(details) = self.details.as_ref().filter(|_| {
            self.details_repository == self.repository
                && settings
                    .active()
                    .is_some_and(|account| account.id == self.details_account)
        }) {
            ui.separator();
            ui.horizontal_wrapped(|ui| {
                ui.heading(format!("#{} {}", details.pull.number, details.pull.title));
                if ui.link("View PR on GitHub").clicked() {
                    events.push(Event::OpenUrl(details.pull.html_url.clone()));
                }
                if ui.button("Checkout PR branch").clicked() {
                    events.push(Event::Checkout(Box::new(details.pull.clone())));
                }
                if ui.button("Refresh").clicked() {
                    events.push(Event::Details(details.pull.number));
                }
                if ui.button("Review…").clicked() {
                    let mut form = Composer::new(ComposeKind::Review);
                    form.number = details.pull.number;
                    form.sha = details.pull.head.sha.clone();
                    form.review_id = self.pending_review;
                    self.composer = Some(form);
                }
                if ui
                    .add_enabled(
                        details.pull.state == "open" && !details.pull.draft && !details.pull.merged,
                        egui::Button::new("Merge…"),
                    )
                    .clicked()
                {
                    let mut form = Composer::new(ComposeKind::Merge);
                    form.number = details.pull.number;
                    form.sha = details.pull.head.sha.clone();
                    form.title = details.pull.title.clone();
                    self.composer = Some(form);
                }
            });
            ui.label(format!(
                "{} → {} · {}{}",
                details.pull.head.name,
                details.pull.base.name,
                details.pull.state,
                if details.pull.draft { " · draft" } else { "" }
            ));
            for warning in &details.warnings {
                ui.colored_label(theme.warning, warning);
            }
            ui.horizontal(|ui| {
                for (index, label) in [
                    "Description",
                    "Commits",
                    "Files / Inline comments",
                    "Timeline / Reviews",
                    "Checks / Actions",
                ]
                .into_iter()
                .enumerate()
                {
                    ui.selectable_value(&mut self.tab, index, label);
                }
            });
            match self.tab {
                0 => {
                    ui.label(details.pull.body.as_deref().unwrap_or_default());
                    ui.horizontal(|ui| {
                        ui.label(format!(
                            "Labels: {}",
                            details
                                .pull
                                .labels
                                .iter()
                                .map(|label| label.name.as_str())
                                .collect::<Vec<_>>()
                                .join(", ")
                        ));
                        ui.label(format!(
                            "Assignees: {}",
                            details
                                .pull
                                .assignees
                                .iter()
                                .map(|user| user.login.as_str())
                                .collect::<Vec<_>>()
                                .join(", ")
                        ));
                    });
                }
                1 => {
                    egui::ScrollArea::vertical()
                        .id_salt("github-commits")
                        .show(ui, |ui| {
                            for commit in &details.commits {
                                if ui
                                    .link(format!(
                                        "{} {}",
                                        &commit.sha[..7.min(commit.sha.len())],
                                        commit.commit.message.lines().next().unwrap_or_default()
                                    ))
                                    .clicked()
                                {
                                    events.push(Event::OpenUrl(commit.html_url.clone()));
                                }
                            }
                        });
                }
                2 => {
                    ui.horizontal_wrapped(|ui| {
                        for (index, file) in details.files.iter().enumerate() {
                            ui.selectable_value(
                                &mut self.selected_file,
                                index,
                                format!(
                                    "{} +{} −{}",
                                    file.filename, file.additions, file.deletions
                                ),
                            );
                        }
                    });
                    if let Some(file) = details.files.get(self.selected_file) {
                        ui.horizontal(|ui| {
                            ui.strong(&file.filename);
                            if ui.link("Jump to source").clicked() {
                                events.push(Event::Navigate {
                                    path: file.filename.clone(),
                                    line: 1,
                                });
                            }
                        });
                        if let Some(patch) = &file.patch {
                            ui.collapsing("Unified diff", |ui| {
                                crate::git_panel::diff_text_ui(ui, theme, patch)
                            });
                            egui::ScrollArea::both()
                                .id_salt("github-diff")
                                .max_height(360.0)
                                .show(ui, |ui| {
                                    for line in diff_lines(patch) {
                                        let text = format!(
                                            "{:>5} {:>5} {}{}",
                                            line.old
                                                .map(|line| line.to_string())
                                                .unwrap_or_default(),
                                            line.new
                                                .map(|line| line.to_string())
                                                .unwrap_or_default(),
                                            line.kind,
                                            line.text
                                        );
                                        let response = ui.selectable_label(
                                            false,
                                            egui::RichText::new(text).monospace().color(match line
                                                .kind
                                            {
                                                '-' => theme.error,
                                                '+' => theme.accent,
                                                _ => theme.text,
                                            }),
                                        );
                                        if response.clicked() {
                                            let mut form = Composer::new(ComposeKind::Inline);
                                            form.number = details.pull.number;
                                            form.sha = details.pull.head.sha.clone();
                                            form.inline = InlineComment {
                                                path: file.filename.clone(),
                                                line: line.new.or(line.old).unwrap_or_default(),
                                                side: if line.new.is_some() {
                                                    "RIGHT"
                                                } else {
                                                    "LEFT"
                                                }
                                                .into(),
                                                body: String::new(),
                                            };
                                            self.composer = Some(form);
                                        }
                                    }
                                });
                        } else {
                            ui.label("This file is binary or its patch was omitted by the server. Open it on GitHub to review it.");
                        }
                        for comment in details
                            .comments
                            .iter()
                            .filter(|comment| comment.path.as_deref() == Some(&file.filename))
                        {
                            ui.group(|ui| {
                                ui.strong(format!(
                                    "{} — line {}",
                                    comment.user.login,
                                    comment.line.or(comment.original_line).unwrap_or_default()
                                ));
                                ui.label(&comment.body);
                                if ui.link("Reply…").clicked() {
                                    let mut form = Composer::new(ComposeKind::Inline);
                                    form.number = details.pull.number;
                                    form.sha = details.pull.head.sha.clone();
                                    form.reply_to = Some(comment.id);
                                    form.inline = InlineComment {
                                        path: file.filename.clone(),
                                        line: comment.line.or(comment.original_line).unwrap_or(1),
                                        side: comment
                                            .side
                                            .clone()
                                            .unwrap_or_else(|| "RIGHT".into()),
                                        body: String::new(),
                                    };
                                    self.composer = Some(form);
                                }
                            });
                        }
                    }
                }
                3 => {
                    egui::ScrollArea::vertical()
                        .id_salt("github-timeline")
                        .show(ui, |ui| {
                            for item in &details.timeline {
                                ui.label(format!(
                                    "{} {} {}",
                                    item.created_at,
                                    item.actor
                                        .as_ref()
                                        .or(item.user.as_ref())
                                        .map(|user| user.login.as_str())
                                        .unwrap_or_default(),
                                    item.event
                                ));
                                if let Some(body) = &item.body {
                                    ui.label(body);
                                }
                            }
                            for review in &details.reviews {
                                ui.group(|ui| {
                                    ui.strong(format!("{}: {}", review.user.login, review.state));
                                    ui.label(&review.body);
                                });
                            }
                            for comment in &details.issue_comments {
                                ui.group(|ui| {
                                    ui.strong(&comment.user.login);
                                    ui.label(&comment.body);
                                });
                            }
                        });
                    if ui.button("Add comment…").clicked() {
                        let mut form = Composer::new(ComposeKind::Comment);
                        form.number = details.pull.number;
                        self.composer = Some(form);
                    }
                }
                _ => {
                    for check in details.checks.iter().chain(if settings.show_actions {
                        details.actions.iter()
                    } else {
                        [].iter()
                    }) {
                        ui.horizontal(|ui| {
                            ui.label(&check.name);
                            ui.label(format!(
                                "{} {}",
                                check.status,
                                check.conclusion.as_deref().unwrap_or_default()
                            ));
                            let url = check.details_url.as_deref().unwrap_or(&check.html_url);
                            if !url.is_empty() {
                                ui.hyperlink_to("Details", url);
                            }
                        });
                    }
                }
            }
        }
        events
    }
    pub fn dialog(
        &mut self,
        ctx: &egui::Context,
        settings: &GitHubSettings,
        busy: bool,
    ) -> (bool, Vec<Event>) {
        let mut events = Vec::new();
        let Some(form) = &mut self.composer else {
            return (false, events);
        };
        let mut open = true;
        let mut submit = false;
        let mut pending = false;
        let mut add = false;
        egui::Window::new(match form.kind {
            ComposeKind::Gist => "Create Gist",
            ComposeKind::Share => "Share Project on GitHub",
            ComposeKind::Pull => "Create Pull Request",
            ComposeKind::Review => "Submit Review",
            ComposeKind::Inline => "Inline Code Review Comment",
            ComposeKind::Comment => "Pull Request Comment",
            ComposeKind::Merge => "Merge Pull Request",
            ComposeKind::Repositories => "Clone from GitHub",
        })
        .open(&mut open)
        .default_width(650.0)
        .show_modal(ctx, |ui| {
            if form.bound && form.kind != ComposeKind::Repositories {
                ui.label(format!(
                    "Account: {} · Repository: {}",
                    form.account.as_deref().unwrap_or("Anonymous"),
                    form.repository
                ));
            }
            if form.kind == ComposeKind::Repositories {
                if settings.accounts.is_empty()
                    && ui.button("Sign in to GitHub / GitLab…").clicked()
                {
                    events.push(Event::Accounts);
                }
                ui.horizontal(|ui| {
                    let organization_label = ui.label("Organization");
                    ui.text_edit_singleline(&mut self.organization);
                    egui::ComboBox::from_id_salt("github-orgs")
                        .selected_text("Choose organization")
                        .show_ui(ui, |ui| {
                            for organization in &self.organizations {
                                ui.selectable_value(
                                    &mut self.organization,
                                    organization.clone(),
                                    organization,
                                );
                            }
                        })
                        .combo_labelled(&organization_label);
                    if ui.button("Load organizations").clicked() {
                        events.push(Event::Organizations);
                    }
                    ui.label("Search");
                    ui.text_edit_singleline(&mut self.repository_search);
                    if ui.button("Search repositories").clicked() {
                        events.push(Event::Repositories);
                    }
                });
                egui::ScrollArea::vertical()
                    .max_height(420.0)
                    .show(ui, |ui| {
                        for repository in &self.repositories {
                            ui.horizontal(|ui| {
                                ui.label(format!(
                                    "{}{}",
                                    repository.full_name,
                                    if repository.private { " (private)" } else { "" }
                                ));
                                if ui.button("Clone…").clicked() {
                                    events.push(Event::Clone(repository.clone_url.clone()));
                                }
                            });
                        }
                    });
                return;
            }
            if form.kind == ComposeKind::Share {
                ui.label("Repository name");
                ui.text_edit_singleline(&mut form.title);
                ui.label("Description");
                ui.text_edit_multiline(&mut form.body);
                ui.label("Organization (optional)");
                ui.text_edit_singleline(&mut form.organization);
                ui.checkbox(&mut form.private, "Private repository");
                ui.horizontal(|ui| {
                    ui.label("Remote name");
                    ui.text_edit_singleline(&mut form.remote);
                });
                ui.checkbox(
                    &mut form.initial,
                    "Make an initial commit of checked files if the repository has no commits",
                );
                if form.initial {
                    egui::ScrollArea::vertical()
                        .max_height(180.0)
                        .show(ui, |ui| {
                            for path in &form.paths {
                                let mut checked = form.checked.contains(path);
                                if ui
                                    .checkbox(&mut checked, path.display().to_string())
                                    .changed()
                                {
                                    if checked {
                                        form.checked.insert(path.clone());
                                    } else {
                                        form.checked.remove(path);
                                    }
                                }
                            }
                        });
                }
            } else if form.kind == ComposeKind::Gist {
                ui.label("Description");
                ui.text_edit_singleline(&mut form.body);
                ui.checkbox(&mut form.private, "Secret gist (unlisted link)");
                ui.checkbox(
                    &mut form.anonymous,
                    "Anonymous (server may refuse anonymous creation)",
                );
                ui.checkbox(&mut form.open_browser, "Open in browser after creation");
                for (name, text) in &form.files {
                    ui.strong(name);
                    egui::ScrollArea::vertical()
                        .max_height(200.0)
                        .show(ui, |ui| {
                            ui.monospace(text.chars().take(8192).collect::<String>());
                        });
                }
            } else if form.kind == ComposeKind::Pull {
                ui.label("Title");
                ui.text_edit_singleline(&mut form.title);
                ui.horizontal(|ui| {
                    ui.label("Head branch");
                    ui.text_edit_singleline(&mut form.head);
                    ui.label("Base branch");
                    ui.text_edit_singleline(&mut form.base);
                });
                if ui
                    .button("Push current branch before creating PR…")
                    .clicked()
                {
                    events.push(Event::PushBranch);
                }
                ui.label(
                    "Create PR uses a published head branch. Push is a separate explicit action.",
                );
                ui.checkbox(&mut form.draft, "Draft PR");
                egui::ComboBox::from_label("Description template")
                    .selected_text("Choose…")
                    .show_ui(ui, |ui| {
                        for (name, text) in &self.templates {
                            if ui.selectable_label(false, name).clicked() {
                                form.body = text.clone();
                            }
                        }
                    });
                ui.label("Description");
                ui.text_edit_multiline(&mut form.body);
                for (label, value) in [
                    ("Reviewers (comma separated)", &mut form.reviewers),
                    ("Assignees", &mut form.assignees),
                    ("Labels", &mut form.labels),
                ] {
                    ui.horizontal(|ui| {
                        ui.label(crate::i18n::tr_static(label));
                        ui.text_edit_singleline(value);
                    });
                }
            } else if form.kind == ComposeKind::Merge {
                ui.label(format!("Merge PR #{} at head {}", form.number, form.sha));
                for method in ["merge", "squash", "rebase"] {
                    if method == "rebase"
                        && settings
                            .active()
                            .is_some_and(|account| account.provider == Provider::GitLab)
                    {
                        continue;
                    }
                    ui.radio_value(
                        &mut form.merge_method,
                        method.into(),
                        crate::i18n::tr_static(method),
                    );
                }
                ui.label("Commit title");
                ui.text_edit_singleline(&mut form.title);
                ui.label("Commit message");
                ui.text_edit_multiline(&mut form.body);
            } else if form.kind == ComposeKind::Inline {
                ui.label(format!(
                    "{}:{} ({})",
                    form.inline.path, form.inline.line, form.inline.side
                ));
                ui.text_edit_multiline(&mut form.inline.body);
                if form.reply_to.is_none() {
                    add = ui.button("Add to pending review").clicked();
                }
            } else {
                if form.kind == ComposeKind::Review {
                    ui.label(format!("Reviewing head {}", form.sha));
                    for (value, label) in [
                        ("COMMENT", "Comment"),
                        ("APPROVE", "Approve"),
                        ("REQUEST_CHANGES", "Request changes"),
                    ] {
                        if value == "REQUEST_CHANGES"
                            && settings
                                .active()
                                .is_some_and(|account| account.provider == Provider::GitLab)
                        {
                            continue;
                        }
                        ui.radio_value(
                            &mut form.review_result,
                            value.into(),
                            crate::i18n::tr_static(label),
                        );
                    }
                    ui.label(format!(
                        "{} pending inline comments",
                        self.pending_comments.len()
                    ));
                    pending = ui
                        .add_enabled(!busy, egui::Button::new("Save as pending review"))
                        .clicked();
                }
                ui.text_edit_multiline(&mut form.body);
            }
            if let Some(error) = &self.error {
                ui.colored_label(ui.visuals().error_fg_color, error);
            }
            submit = ui
                .add_enabled(
                    !busy,
                    egui::Button::new(match form.kind {
                        ComposeKind::Share => "Create repository and publish checked project files",
                        ComposeKind::Gist => "Create Gist",
                        ComposeKind::Pull => "Create Pull Request",
                        ComposeKind::Merge => "Merge this head commit",
                        ComposeKind::Review => "Submit Review",
                        _ => "Post Comment",
                    }),
                )
                .clicked();
        });
        if add {
            if let Err(error) = form.inline.validate() {
                self.error = Some(error);
            } else {
                self.pending_comments.push(form.inline.clone());
                self.composer = None;
                return (false, events);
            }
        }
        if submit || pending {
            let form = self.composer.as_ref().unwrap();
            let operation = match form.kind {
                ComposeKind::Share => Operation::CreateRepository {
                    name: form.title.clone(),
                    description: form.body.clone(),
                    private: form.private,
                    organization: form.organization.clone(),
                },
                ComposeKind::Gist => Operation::CreateGist {
                    description: form.body.clone(),
                    public: !form.private,
                    files: form.files.clone(),
                },
                ComposeKind::Pull => Operation::CreatePull(NewPull {
                    title: form.title.clone(),
                    body: form.body.clone(),
                    head: form.head.clone(),
                    base: form.base.clone(),
                    draft: form.draft,
                    reviewers: names(&form.reviewers),
                    assignees: names(&form.assignees),
                    labels: names(&form.labels),
                }),
                ComposeKind::Review => {
                    if let Some(review) = form.review_id {
                        Operation::SubmitReview {
                            number: form.number,
                            review,
                            event: form.review_result.clone(),
                            body: form.body.clone(),
                        }
                    } else {
                        Operation::Review {
                            number: form.number,
                            sha: form.sha.clone(),
                            body: form.body.clone(),
                            event: if pending {
                                String::new()
                            } else {
                                form.review_result.clone()
                            },
                            comments: self.pending_comments.clone(),
                        }
                    }
                }
                ComposeKind::Inline => Operation::InlineComment {
                    number: form.number,
                    comment: form.inline.clone(),
                    sha: form.sha.clone(),
                    reply_to: form.reply_to,
                },
                ComposeKind::Comment => Operation::Comment {
                    number: form.number,
                    body: form.body.clone(),
                },
                ComposeKind::Merge => Operation::Merge {
                    number: form.number,
                    sha: form.sha.clone(),
                    method: form.merge_method.clone(),
                    title: form.title.clone(),
                    message: form.body.clone(),
                },
                ComposeKind::Repositories => return (open, events),
            };
            if let Err(error) = operation.plan(&self.repository) {
                self.error = Some(error);
            } else if form.kind == ComposeKind::Share {
                events.push(Event::Share {
                    operation,
                    remote: form.remote.clone(),
                    initial: form.initial,
                    files: form.checked.iter().cloned().collect(),
                });
            } else {
                events.push(Event::Write(operation));
            }
        }
        if !open {
            self.composer = None;
        }
        (open, events)
    }
}

/// Issue navigation uses the selected server, including Enterprise instances.
pub fn issue_urls(server: &Server, repository: &str, text: &str) -> Vec<String> {
    let expression = regex::Regex::new(
        r"(?:(?P<repo>[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+))?#(?P<number>[1-9][0-9]*)",
    )
    .expect("static issue pattern");
    expression
        .captures_iter(text)
        .map(|capture| {
            format!(
                "{}/{}/{}issues/{}",
                server.web,
                capture
                    .name("repo")
                    .map(|repo| repo.as_str())
                    .unwrap_or(repository),
                if server.provider == Provider::GitLab {
                    "-/"
                } else {
                    ""
                },
                &capture["number"]
            )
        })
        .collect()
}

#[cfg(test)]
mod fixtures {
    use super::*;
    #[test]
    fn gist_toolbar_opens_the_existing_explicit_composer_route() {
        let context = egui::Context::default();
        let mut workspace = Workspace {
            last_gist_url: Some("https://gist.github.com/example/1".into()),
            ..Default::default()
        };
        let settings = GitHubSettings::default();
        let output = context.run_ui(egui::RawInput::default(), |ui| {
            workspace.ui(ui, &UiTheme::dark(), &settings, false);
        });
        let text_rect = |label: &str| {
            output.shapes.iter().find_map(|shape| match &shape.shape {
                egui::epaint::Shape::Text(text) if text.galley.job.text == label => {
                    Some(egui::Rect::from_min_size(text.pos, text.galley.size()))
                }
                _ => None,
            })
        };
        let last_created_gist = text_rect("Last created Gist").is_some();
        let create_gist = text_rect("Create Gist…");
        output.drop_without_applying_deltas();
        assert!(last_created_gist);
        let position = create_gist.expect("actual toolbar button").center();
        let input = egui::RawInput {
            events: vec![
                egui::Event::PointerMoved(position),
                egui::Event::PointerButton {
                    pos: position,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
                egui::Event::PointerButton {
                    pos: position,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
            ..Default::default()
        };
        let mut events = Vec::new();
        let output = context.run_ui(input, |ui| {
            events.extend(workspace.ui(ui, &UiTheme::dark(), &settings, false));
        });
        output.drop_without_applying_deltas();
        assert!(events.iter().any(|event| matches!(event, Event::NewGist)));
        assert!(
            workspace.composer.is_none(),
            "the captured app route owns composer admission"
        );
    }
    #[test]
    fn issue_links_keep_enterprise_and_gitlab_authority() {
        let server = Server::new("https://forge.example", Provider::GitHub).unwrap();
        assert_eq!(
            issue_urls(&server, "team/repo", "Fix #12 and other/repo#8"),
            vec![
                "https://forge.example/team/repo/issues/12",
                "https://forge.example/other/repo/issues/8"
            ]
        );
        let gitlab = Server::new("https://gitlab.example", Provider::GitLab).unwrap();
        assert_eq!(
            issue_urls(&gitlab, "group/nested/repo", "#1"),
            vec!["https://gitlab.example/group/nested/repo/-/issues/1"]
        );
    }
}

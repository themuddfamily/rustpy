//! Account metadata and pull-request preferences; tokens live in the password safe.
use serde::{Deserialize, Serialize};
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Provider {
    #[default]
    GitHub,
    GitLab,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Account {
    pub id: String,
    pub server: String,
    pub login: String,
    pub name: String,
    pub scopes: Vec<String>,
    pub provider: Provider,
}
impl Account {
    pub fn secret_key(&self) -> String {
        format!("forge/{}/{}/token", self.server, self.id)
    }
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GitHubSettings {
    pub accounts: Vec<Account>,
    pub active_account: Option<String>,
    pub oauth_client_id: String,
    pub notify: bool,
    pub show_review_gutter: bool,
    pub refresh_seconds: u64,
    pub default_draft: bool,
    pub pull_request_template: String,
    pub show_actions: bool,
}
impl Default for GitHubSettings {
    fn default() -> Self {
        Self {
            accounts: Vec::new(),
            active_account: None,
            oauth_client_id: String::new(),
            notify: true,
            show_review_gutter: true,
            refresh_seconds: 120,
            default_draft: false,
            pull_request_template: String::new(),
            show_actions: true,
        }
    }
}
impl GitHubSettings {
    pub fn active(&self) -> Option<&Account> {
        self.accounts
            .iter()
            .find(|account| self.active_account.as_ref() == Some(&account.id))
            .or(self.accounts.first())
    }
}

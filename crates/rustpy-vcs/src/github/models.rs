use serde::{Deserialize, Serialize};
use serde_json::Value;
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct User {
    pub id: u64,
    #[serde(alias = "username")]
    pub login: String,
    pub name: Option<String>,
    pub avatar_url: String,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Repository {
    pub id: u64,
    pub name: String,
    #[serde(alias = "path_with_namespace")]
    pub full_name: String,
    pub description: Option<String>,
    pub private: bool,
    pub html_url: String,
    pub clone_url: String,
    pub ssh_url: String,
    pub default_branch: String,
    pub owner: User,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Ref {
    pub label: String,
    #[serde(rename = "ref")]
    pub name: String,
    pub sha: String,
    pub repo: Option<Repository>,
    pub user: User,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct PullRequest {
    pub id: u64,
    pub node_id: String,
    #[serde(alias = "iid")]
    pub number: u64,
    pub title: String,
    #[serde(alias = "description")]
    pub body: Option<String>,
    pub state: String,
    pub draft: bool,
    pub html_url: String,
    pub user: User,
    pub head: Ref,
    pub base: Ref,
    pub labels: Vec<Label>,
    pub assignees: Vec<User>,
    pub requested_reviewers: Vec<User>,
    pub mergeable: Option<bool>,
    pub mergeable_state: String,
    pub merged: bool,
    pub updated_at: String,
    pub diff_refs: Value,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Label {
    pub name: String,
    pub color: String,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct PullFile {
    #[serde(alias = "new_path")]
    pub filename: String,
    #[serde(alias = "old_path")]
    pub previous_filename: Option<String>,
    pub status: String,
    pub additions: usize,
    pub deletions: usize,
    pub patch: Option<String>,
    pub sha: String,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Comment {
    pub id: u64,
    pub body: String,
    pub user: User,
    pub html_url: String,
    pub created_at: String,
    pub path: Option<String>,
    pub line: Option<usize>,
    pub original_line: Option<usize>,
    pub side: Option<String>,
    pub commit_id: Option<String>,
    pub pull_request_review_id: Option<u64>,
    pub in_reply_to_id: Option<u64>,
    pub position: Option<Value>,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Review {
    pub id: u64,
    pub body: String,
    pub state: String,
    pub user: User,
    pub commit_id: String,
    pub html_url: String,
    pub submitted_at: String,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Commit {
    pub sha: String,
    pub html_url: String,
    pub commit: CommitData,
    pub author: Option<User>,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct CommitData {
    pub message: String,
    pub author: CommitAuthor,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct CommitAuthor {
    pub name: String,
    pub date: String,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Check {
    pub id: u64,
    pub name: String,
    pub status: String,
    pub conclusion: Option<String>,
    pub html_url: String,
    pub details_url: Option<String>,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct TimelineEvent {
    pub id: u64,
    pub event: String,
    pub body: Option<String>,
    pub created_at: String,
    pub actor: Option<User>,
    pub user: Option<User>,
    pub html_url: String,
    pub state: Option<String>,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Notification {
    pub id: String,
    pub unread: bool,
    pub reason: String,
    pub repository: Repository,
    pub subject: Value,
    pub updated_at: String,
}
#[derive(Clone, Debug, Default)]
pub struct PullDetails {
    pub pull: PullRequest,
    pub files: Vec<PullFile>,
    pub commits: Vec<Commit>,
    pub comments: Vec<Comment>,
    pub issue_comments: Vec<Comment>,
    pub reviews: Vec<Review>,
    pub timeline: Vec<TimelineEvent>,
    pub checks: Vec<Check>,
    pub actions: Vec<Check>,
    pub warnings: Vec<String>,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Search {
    pub state: String,
    pub author: String,
    pub assignee: String,
    pub review: String,
    pub label: String,
    pub text: String,
}
impl Search {
    pub fn github_query(&self, repository: &str) -> String {
        let mut query = format!("repo:{repository} is:pr");
        if self.state == "merged" {
            query.push_str(" is:merged");
        }
        for (name, value) in [
            ("state", self.state.as_str()),
            ("author", &self.author),
            ("assignee", &self.assignee),
            ("review", &self.review),
            ("label", &self.label),
        ] {
            if !value.trim().is_empty() && value != "all" && !(name == "state" && value == "merged")
            {
                query.push(' ');
                query.push_str(name);
                query.push(':');
                query.push_str(&quote(value));
            }
        }
        if !self.text.trim().is_empty() {
            query.push(' ');
            query.push_str(self.text.trim());
        }
        query
    }
}
fn quote(value: &str) -> String {
    if value.chars().any(char::is_whitespace) {
        format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
    } else {
        value.into()
    }
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Issue {
    pub id: u64,
    pub number: u64,
    pub title: String,
    pub body: Option<String>,
    pub state: String,
    pub html_url: String,
    pub user: User,
    pub labels: Vec<Label>,
    pub updated_at: String,
    pub assignees: Vec<User>,
}

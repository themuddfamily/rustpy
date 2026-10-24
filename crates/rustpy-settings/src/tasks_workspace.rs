//! Project task preferences and tracker definitions. Credentials live in Password Safe.
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum TrackerKind {
    #[default]
    YouTrack,
    Jira,
    GitHub,
    GitLab,
    Bugzilla,
    Trello,
    Redmine,
    Mantis,
    Lighthouse,
    Pivotal,
    Asana,
    Trac,
    FogBugz,
    Generic,
}
impl TrackerKind {
    pub const ALL: &'static [Self] = &[
        Self::YouTrack,
        Self::Jira,
        Self::GitHub,
        Self::GitLab,
        Self::Bugzilla,
        Self::Trello,
        Self::Redmine,
        Self::Mantis,
        Self::Lighthouse,
        Self::Pivotal,
        Self::Asana,
        Self::Trac,
        Self::FogBugz,
        Self::Generic,
    ];
    pub fn label(self) -> &'static str {
        match self {
            Self::YouTrack => "YouTrack",
            Self::Jira => "Jira",
            Self::GitHub => "GitHub",
            Self::GitLab => "GitLab",
            Self::Bugzilla => "Bugzilla",
            Self::Trello => "Trello",
            Self::Redmine => "Redmine",
            Self::Mantis => "Mantis",
            Self::Lighthouse => "Lighthouse",
            Self::Pivotal => "Pivotal Tracker",
            Self::Asana => "Asana",
            Self::Trac => "Trac JSON-RPC",
            Self::FogBugz => "FogBugz",
            Self::Generic => "Generic REST",
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Tracker {
    pub id: String,
    pub name: String,
    pub kind: TrackerKind,
    pub url: String,
    /// Repository owner/name, GitLab project path, project id, or Asana workspace gid.
    pub project: String,
    pub username: String,
    /// Trello developer API key; its access token remains in Password Safe.
    pub api_key: String,
    /// Shared GitHub/GitLab account authority, rather than a second stored token.
    pub account_id: Option<String>,
    pub search: String,
    pub generic: GenericRest,
}
impl Default for Tracker {
    fn default() -> Self {
        Self {
            id: String::new(),
            name: String::new(),
            kind: TrackerKind::YouTrack,
            url: String::new(),
            project: String::new(),
            username: String::new(),
            api_key: String::new(),
            account_id: None,
            search: String::new(),
            generic: GenericRest::default(),
        }
    }
}
impl Tracker {
    pub fn secret_key(&self) -> String {
        format!("tasks/tracker/{}", self.id)
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct GenericRest {
    pub search_path: String,
    pub items_pointer: String,
    pub id_pointer: String,
    pub title_pointer: String,
    pub url_pointer: String,
    pub browser_pattern: String,
    pub auth_header: String,
    pub auth_prefix: String,
    pub worklog_path: String,
    pub worklog_json: String,
}
impl Default for GenericRest {
    fn default() -> Self {
        Self {
            search_path: "/issues?q=${query}".into(),
            items_pointer: "/issues".into(),
            id_pointer: "/id".into(),
            title_pointer: "/title".into(),
            url_pointer: "/url".into(),
            browser_pattern: "${server}/issues/${id}".into(),
            auth_header: "Authorization".into(),
            auth_prefix: "Bearer ".into(),
            worklog_path: String::new(),
            worklog_json:
                r#"{"issue":${id_json},"seconds":${seconds},"description":${description_json}}"#
                    .into(),
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Preferences {
    pub trackers: Vec<Tracker>,
    pub changelist_pattern: String,
    pub branch_pattern: String,
    pub commit_pattern: String,
    pub create_changelist: bool,
    pub create_branch: bool,
    pub capture_tools: bool,
    pub capture_breakpoints: bool,
    pub tracking: bool,
    pub issue_navigation: Vec<IssueNavigation>,
}
impl Default for Preferences {
    fn default() -> Self {
        Self {
            trackers: vec![],
            changelist_pattern: "${id} ${summary}".into(),
            branch_pattern: "${id}-${summary}".into(),
            commit_pattern: "${id} ${summary}".into(),
            create_changelist: true,
            create_branch: false,
            capture_tools: true,
            capture_breakpoints: false,
            tracking: true,
            issue_navigation: vec![],
        }
    }
}
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct IssueNavigation {
    pub pattern: String,
    pub replacement: String,
}
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct WorkItem {
    pub id: u64,
    pub started: u64,
    pub seconds: u64,
    pub description: String,
    pub sent: bool,
    #[serde(skip)]
    pub sending: bool,
    /// Identity of the live request; reloading a task store never adopts it.
    #[serde(skip)]
    pub sending_owner: Option<u64>,
    pub receipt: Option<String>,
}

pub fn expand(pattern: &str, id: &str, summary: &str) -> String {
    pattern.replace("${id}", id).replace("${summary}", summary)
}
pub fn branch(pattern: &str, id: &str, summary: &str) -> String {
    let expanded = expand(pattern, id, summary);
    let mut slug = String::new();
    for c in expanded.chars().take(160) {
        if c.is_ascii_alphanumeric() || c == '/' || c == '_' {
            slug.push(c.to_ascii_lowercase());
        } else if !slug.ends_with('-') {
            slug.push('-');
        }
    }
    slug.split('/')
        .map(|part| part.trim_matches('-'))
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("/")
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn old_preferences_are_safe() {
        let p: Preferences = serde_json::from_str("{}").unwrap();
        assert!(!p.capture_breakpoints);
        assert!(p.tracking);
        assert_eq!(
            branch("task/${id}-${summary}", "#42", "Fix ../ crash"),
            "task/42-fix/crash"
        );
    }
    #[test]
    fn credentials_not_in_config() {
        let t = Tracker {
            id: "stable".into(),
            ..Default::default()
        };
        assert_eq!(t.secret_key(), "tasks/tracker/stable");
        assert!(!serde_json::to_string(&t).unwrap().contains("token"));
    }
}

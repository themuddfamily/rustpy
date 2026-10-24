//! Typed request plans, shared by UI actions and unexecuted protocol fixtures.
use super::{client::*, models::*};
use rustpy_core::CancellationToken;
use rustpy_settings::github::Provider;
use serde_json::{json, Value};
#[derive(Clone, Debug, Default)]
pub struct NewPull {
    pub title: String,
    pub body: String,
    pub head: String,
    pub base: String,
    pub draft: bool,
    pub reviewers: Vec<String>,
    pub assignees: Vec<String>,
    pub labels: Vec<String>,
}
#[derive(Clone, Debug)]
pub enum Operation {
    CreateRepository {
        name: String,
        description: String,
        private: bool,
        organization: String,
    },
    CreateGist {
        description: String,
        public: bool,
        files: std::collections::BTreeMap<String, String>,
    },
    CreatePull(NewPull),
    Review {
        number: u64,
        sha: String,
        body: String,
        event: String,
        comments: Vec<InlineComment>,
    },
    SubmitReview {
        number: u64,
        review: u64,
        event: String,
        body: String,
    },
    InlineComment {
        number: u64,
        comment: InlineComment,
        sha: String,
        reply_to: Option<u64>,
    },
    Comment {
        number: u64,
        body: String,
    },
    Merge {
        number: u64,
        sha: String,
        method: String,
        title: String,
        message: String,
    },
    Reviewers {
        number: u64,
        reviewers: Vec<String>,
    },
    Metadata {
        number: u64,
        assignees: Vec<String>,
        labels: Vec<String>,
    },
}
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct InlineComment {
    pub path: String,
    pub line: usize,
    pub side: String,
    pub body: String,
}
impl InlineComment {
    pub fn validate(&self) -> Result<(), String> {
        if self.path.is_empty()
            || self.path.starts_with('/')
            || self.path.split('/').any(|part| part == "..")
            || self.line == 0
            || !matches!(self.side.as_str(), "LEFT" | "RIGHT")
            || self.body.trim().is_empty()
        {
            return Err("Choose a changed diff line and enter a comment".into());
        }
        Ok(())
    }
}
pub fn repository_path(repository: &str) -> Result<String, String> {
    if repository.split('/').count() < 2
        || repository
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
        || repository.contains(['?', '#', '\\'])
        || repository.chars().any(char::is_whitespace)
    {
        return Err("Choose owner/repository".into());
    }
    Ok(format!(
        "/repos/{}",
        repository
            .split('/')
            .map(encode)
            .collect::<Vec<_>>()
            .join("/")
    ))
}
impl Operation {
    pub fn plan(&self, repository: &str) -> Result<Request, String> {
        let base = if matches!(
            self,
            Self::CreateRepository { .. } | Self::CreateGist { .. }
        ) {
            String::new()
        } else {
            repository_path(repository)?
        };
        Ok(match self {
            Self::CreateRepository {
                name,
                description,
                private,
                organization,
            } => {
                if name.trim().is_empty() || name.contains(['/', '\\']) {
                    return Err("Enter a repository name".into());
                }
                Request::write(
                    "POST",
                    if organization.trim().is_empty() {
                        "/user/repos".into()
                    } else {
                        format!("/orgs/{}/repos", encode(organization))
                    },
                    json!({"name":name,"description":description,"private":private,"auto_init":false}),
                )
            }
            Self::CreateGist {
                description,
                public,
                files,
            } => {
                if files.is_empty()
                    || files.values().all(|text| text.is_empty())
                    || files.iter().any(|(name, text)| {
                        name.is_empty() || name.contains(['/', '\\']) || text.len() > 1024 * 1024
                    })
                {
                    return Err("Choose a nonempty text file of at most 1 MiB".into());
                }
                let files: serde_json::Map<String, Value> = files
                    .iter()
                    .map(|(name, content)| (name.clone(), json!({"content":content})))
                    .collect();
                Request::write(
                    "POST",
                    "/gists",
                    json!({"description":description,"public":public,"files":files}),
                )
            }
            Self::CreatePull(pull) => {
                if pull.title.trim().is_empty()
                    || pull.head.trim().is_empty()
                    || pull.base.trim().is_empty()
                {
                    return Err("Enter title, source branch and target branch".into());
                }
                Request::write(
                    "POST",
                    format!("{base}/pulls"),
                    json!({"title":pull.title,"body":pull.body,"head":pull.head,"base":pull.base,"draft":pull.draft}),
                )
            }
            Self::Review {
                number,
                sha,
                body,
                event,
                comments,
            } => {
                if sha.is_empty()
                    || !matches!(
                        event.as_str(),
                        "" | "APPROVE" | "REQUEST_CHANGES" | "COMMENT"
                    )
                {
                    return Err("Choose a review result for the current head commit".into());
                }
                if event != "APPROVE" && body.trim().is_empty() && comments.is_empty() {
                    return Err("Enter review feedback".into());
                }
                for comment in comments {
                    comment.validate()?;
                }
                let mut payload = json!({"commit_id":sha,"body":body,"comments":comments});
                if !event.is_empty() {
                    payload["event"] = json!(event);
                }
                Request::write("POST", format!("{base}/pulls/{number}/reviews"), payload)
            }
            Self::SubmitReview {
                number,
                review,
                event,
                body,
            } => {
                if !matches!(event.as_str(), "APPROVE" | "REQUEST_CHANGES" | "COMMENT") {
                    return Err("Invalid review state".into());
                }
                Request::write(
                    "POST",
                    format!("{base}/pulls/{number}/reviews/{review}/events"),
                    json!({"event":event,"body":body}),
                )
            }
            Self::InlineComment {
                number,
                comment,
                sha,
                reply_to,
            } => {
                comment.validate()?;
                if let Some(reply) = reply_to {
                    Request::write(
                        "POST",
                        format!("{base}/pulls/{number}/comments/{reply}/replies"),
                        json!({"body":comment.body}),
                    )
                } else {
                    if sha.is_empty() {
                        return Err("Refresh the pull request head before commenting".into());
                    }
                    Request::write(
                        "POST",
                        format!("{base}/pulls/{number}/comments"),
                        json!({"body":comment.body,"commit_id":sha,"path":comment.path,"line":comment.line,"side":comment.side}),
                    )
                }
            }
            Self::Comment { number, body } => {
                if body.trim().is_empty() {
                    return Err("Enter a comment".into());
                }
                Request::write(
                    "POST",
                    format!("{base}/issues/{number}/comments"),
                    json!({"body":body}),
                )
            }
            Self::Merge {
                number,
                sha,
                method,
                title,
                message,
            } => {
                if sha.is_empty() || !matches!(method.as_str(), "merge" | "squash" | "rebase") {
                    return Err("Refresh the pull request and choose a merge method".into());
                }
                Request::write(
                    "PUT",
                    format!("{base}/pulls/{number}/merge"),
                    json!({"sha":sha,"merge_method":method,"commit_title":title,"commit_message":message}),
                )
            }
            Self::Reviewers { number, reviewers } => Request::write(
                "POST",
                format!("{base}/pulls/{number}/requested_reviewers"),
                json!({"reviewers":reviewers}),
            ),
            Self::Metadata {
                number,
                assignees,
                labels,
            } => Request::write(
                "PATCH",
                format!("{base}/issues/{number}"),
                json!({"assignees":assignees,"labels":labels}),
            ),
        })
    }
}
impl<T: Transport> Client<T> {
    pub fn repositories(
        &self,
        organization: &str,
        search: &str,
        cancel: &CancellationToken,
    ) -> Result<Vec<Repository>, String> {
        if self.server.provider == Provider::GitLab {
            return super::gitlab::repositories(self, organization, search, cancel);
        }
        let path = if !organization.trim().is_empty() {
            format!("/orgs/{}/repos?type=all&sort=updated", encode(organization))
        } else if !search.trim().is_empty() {
            format!("/search/repositories?q={}", encode(search))
        } else {
            "/user/repos?affiliation=owner,collaborator,organization_member&sort=updated".into()
        };
        let key = (!search.trim().is_empty() && organization.trim().is_empty()).then_some("items");
        self.pages(&path, key, cancel)?
            .into_iter()
            .map(|value| serde_json::from_value(value).map_err(|error| error.to_string()))
            .collect()
    }
    pub fn pulls(
        &self,
        repository: &str,
        search: &Search,
        cancel: &CancellationToken,
    ) -> Result<Vec<PullRequest>, String> {
        if self.server.provider == Provider::GitLab {
            return super::gitlab::pulls(self, repository, search, cancel);
        }
        repository_path(repository)?;
        let query = search.github_query(repository);
        self.pages(
            &format!("/search/issues?q={}&sort=updated", encode(&query)),
            Some("items"),
            cancel,
        )?
        .into_iter()
        .map(|value| serde_json::from_value(value).map_err(|error| error.to_string()))
        .collect()
    }
    pub fn details(
        &self,
        repository: &str,
        number: u64,
        cancel: &CancellationToken,
    ) -> Result<PullDetails, String> {
        if self.server.provider == Provider::GitLab {
            return super::gitlab::details(self, repository, number, cancel);
        }
        let base = repository_path(repository)?;
        let prefix = format!("{base}/pulls/{number}");
        let pull: PullRequest =
            serde_json::from_value(self.send(&Request::read(&prefix), cancel)?.value)
                .map_err(|error| error.to_string())?;
        let mut result = PullDetails {
            pull,
            ..Default::default()
        };
        macro_rules! load {
            ($field:ident,$path:expr,$key:expr) => {
                match self.pages(&$path, $key, cancel).and_then(|values| {
                    values
                        .into_iter()
                        .map(|value| {
                            serde_json::from_value(value).map_err(|error| error.to_string())
                        })
                        .collect()
                }) {
                    Ok(values) => result.$field = values,
                    Err(error) => result
                        .warnings
                        .push(format!("{}: {error}", stringify!($field))),
                }
            };
        }
        load!(files, format!("{prefix}/files"), None);
        load!(commits, format!("{prefix}/commits"), None);
        load!(comments, format!("{prefix}/comments"), None);
        load!(
            issue_comments,
            format!("{base}/issues/{number}/comments"),
            None
        );
        load!(reviews, format!("{prefix}/reviews"), None);
        load!(timeline, format!("{base}/issues/{number}/timeline"), None);
        load!(
            checks,
            format!(
                "{base}/commits/{}/check-runs",
                encode(&result.pull.head.sha)
            ),
            Some("check_runs")
        );
        match self.send(
            &Request::read(format!(
                "{base}/commits/{}/status",
                encode(&result.pull.head.sha)
            )),
            cancel,
        ) {
            Ok(response) => {
                if let Some(statuses) = response.value["statuses"].as_array() {
                    for status in statuses {
                        result.checks.push(Check {
                            id: status["id"].as_u64().unwrap_or_default(),
                            name: status["context"].as_str().unwrap_or("Commit status").into(),
                            status: status["state"].as_str().unwrap_or("unknown").into(),
                            conclusion: status["state"].as_str().map(str::to_owned),
                            html_url: status["target_url"].as_str().unwrap_or_default().into(),
                            details_url: None,
                        });
                    }
                }
            }
            Err(error) => result.warnings.push(format!("Commit statuses: {error}")),
        }
        let runs = self.pages(
            &format!(
                "{base}/actions/runs?head_sha={}",
                encode(&result.pull.head.sha)
            ),
            Some("workflow_runs"),
            cancel,
        );
        match runs {
            Ok(values) => {
                result.actions = values
                    .into_iter()
                    .filter_map(|value| serde_json::from_value(value).ok())
                    .collect()
            }
            Err(error) => result.warnings.push(format!("Actions: {error}")),
        };
        Ok(result)
    }
    pub fn execute(
        &self,
        repository: &str,
        operation: &Operation,
        cancel: &CancellationToken,
    ) -> Result<Value, String> {
        if self.server.provider == Provider::GitLab {
            operation.plan(repository)?;
            return super::gitlab::execute(self, repository, operation, cancel);
        }
        let value = self.send(&operation.plan(repository)?, cancel)?.value;
        if matches!(operation, Operation::Merge { .. }) && value["merged"] == false {
            return Err(value["message"]
                .as_str()
                .unwrap_or("The server declined this merge")
                .into());
        }
        if let Operation::CreatePull(pull) = operation {
            let number = value["number"]
                .as_u64()
                .ok_or("Created pull request response has no number")?;
            let mut failures = Vec::new();
            if !pull.reviewers.is_empty() {
                if let Err(error) = self.send(
                    &Operation::Reviewers {
                        number,
                        reviewers: pull.reviewers.clone(),
                    }
                    .plan(repository)?,
                    cancel,
                ) {
                    failures.push(error);
                }
            }
            if !pull.assignees.is_empty() || !pull.labels.is_empty() {
                if let Err(error) = self.send(
                    &Operation::Metadata {
                        number,
                        assignees: pull.assignees.clone(),
                        labels: pull.labels.clone(),
                    }
                    .plan(repository)?,
                    cancel,
                ) {
                    failures.push(error);
                }
            }
            if !failures.is_empty() {
                let mut value = value;
                value["rustpy_metadata_warnings"] = json!(failures);
                return Ok(value);
            }
        }
        Ok(value)
    }
}
/// A diff line is a commentable location only when emitted by a real hunk.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffLine {
    pub old: Option<usize>,
    pub new: Option<usize>,
    pub text: String,
    pub kind: char,
}
pub fn diff_lines(patch: &str) -> Vec<DiffLine> {
    let mut old = 0usize;
    let mut new = 0usize;
    let mut in_hunk = false;
    let mut result = Vec::new();
    for text in patch.lines() {
        if text.starts_with("@@ ") {
            let mut words = text.split_whitespace();
            words.next();
            old = words
                .next()
                .and_then(|word| word.trim_start_matches('-').split(',').next())
                .and_then(|line| line.parse().ok())
                .unwrap_or(0);
            new = words
                .next()
                .and_then(|word| word.trim_start_matches('+').split(',').next())
                .and_then(|line| line.parse().ok())
                .unwrap_or(0);
            in_hunk = true;
            continue;
        }
        if !in_hunk {
            continue;
        }
        match text.chars().next() {
            Some('+') => {
                result.push(DiffLine {
                    old: None,
                    new: Some(new),
                    text: text[1..].into(),
                    kind: '+',
                });
                new += 1;
            }
            Some('-') => {
                result.push(DiffLine {
                    old: Some(old),
                    new: None,
                    text: text[1..].into(),
                    kind: '-',
                });
                old += 1;
            }
            Some(' ') => {
                result.push(DiffLine {
                    old: Some(old),
                    new: Some(new),
                    text: text[1..].into(),
                    kind: ' ',
                });
                old += 1;
                new += 1;
            }
            _ => {}
        }
    }
    result
}
impl<T: Transport> Client<T> {
    /// GitLab records spent time natively. GitHub has no equivalent endpoint.
    pub fn issue_spent_time(
        &self,
        repository: &str,
        number: u64,
        seconds: u64,
        description: &str,
        cancel: &CancellationToken,
    ) -> Result<Value, String> {
        repository_path(repository)?;
        if self.server.provider != Provider::GitLab {
            return Err("GitHub does not support native issue worklogs".into());
        }
        if number == 0 || seconds == 0 {
            return Err("An issue and positive duration are required".into());
        }
        self.send(
            &Request::write(
                "POST",
                format!(
                    "/projects/{}/issues/{number}/add_spent_time",
                    encode(repository)
                ),
                json!({"duration":format!("{seconds}s"),"summary":description}),
            ),
            cancel,
        )
        .map(|response| response.value)
    }
    /// Shared authenticated issue search for Tasks and Contexts. Pull requests
    /// are excluded at both the query and decoder boundaries.
    pub fn issues(
        &self,
        repository: &str,
        query: &str,
        cancel: &CancellationToken,
    ) -> Result<Vec<Issue>, String> {
        repository_path(repository)?;
        if self.server.provider == Provider::GitLab {
            let path = format!(
                "/projects/{}/issues?scope=all&search={}",
                encode(repository),
                encode(query)
            );
            return self
                .pages(&path, None, cancel)?
                .into_iter()
                .map(|mut value| {
                    value["number"] = value["iid"].clone();
                    value["html_url"] = value["web_url"].clone();
                    value["user"] = value["author"].clone();
                    value["body"] = value["description"].clone();
                    value["labels"] = json!(value["labels"]
                        .as_array()
                        .map(|labels| labels
                            .iter()
                            .map(|label| json!({"name":label}))
                            .collect::<Vec<_>>())
                        .unwrap_or_default());
                    serde_json::from_value(value).map_err(|error| error.to_string())
                })
                .collect();
        }
        let query = format!("repo:{repository} is:issue {query}");
        self.pages(
            &format!("/search/issues?q={}", encode(&query)),
            Some("items"),
            cancel,
        )?
        .into_iter()
        .filter(|value| value.get("pull_request").is_none())
        .map(|value| serde_json::from_value(value).map_err(|error| error.to_string()))
        .collect()
    }
}

//! Recorded protocol fixtures. These tests require no Git process or network.
use super::*;
use rustpy_core::CancellationToken;
use rustpy_settings::github::Provider;
use serde_json::json;
use std::sync::{Arc, Mutex};

struct Recorded {
    requests: Arc<Mutex<Vec<Request>>>,
    replies: Mutex<std::collections::VecDeque<Response>>,
}
impl Transport for Recorded {
    fn send(
        &self,
        _server: &Server,
        token: &str,
        request: &Request,
        cancel: &CancellationToken,
    ) -> Result<Response, String> {
        cancel.check().map_err(|error| error.to_string())?;
        assert_eq!(token, "fixture-token");
        self.requests.lock().unwrap().push(request.clone());
        self.replies
            .lock()
            .unwrap()
            .pop_front()
            .ok_or("Unexpected request".into())
    }
}
fn recorded(
    provider: Provider,
    values: Vec<(serde_json::Value, Option<usize>)>,
) -> (Client<Recorded>, Arc<Mutex<Vec<Request>>>) {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let transport = Recorded {
        requests: requests.clone(),
        replies: Mutex::new(
            values
                .into_iter()
                .map(|(value, next_page)| Response {
                    value,
                    next_page,
                    scopes: Vec::new(),
                })
                .collect(),
        ),
    };
    (
        Client::new(
            Server::new("https://forge.example", provider).unwrap(),
            "fixture-token".into(),
            transport,
        ),
        requests,
    )
}
#[test]
fn origin_and_enterprise_paths_cannot_escape_configured_server() {
    assert!(Server::new("https://user@forge.example", Provider::GitHub).is_err());
    assert!(Server::new("https://forge.example/api", Provider::GitHub).is_err());
    let server = Server::new("https://forge.example", Provider::GitHub).unwrap();
    assert_eq!(
        server.api_url("/user").unwrap(),
        "https://forge.example/api/v3/user"
    );
    assert!(server.api_url("//attacker.example/user").is_err());
    assert!(server.api_url("/../user").is_err());
    assert!(server
        .remote_repository("https://forge.example.attacker/repo/name.git")
        .is_none());
    assert_eq!(
        server
            .remote_repository("git@forge.example:team/repo.git")
            .as_deref(),
        Some("team/repo")
    );
    assert_eq!(
        server.file_url("team/repo", "feature/a", "a b.py", Some(2), Some(4)),
        "https://forge.example/team/repo/blob/feature%2Fa/a%20b.py#L2-L4"
    );
    let gitlab = Server::new("https://forge.example", Provider::GitLab).unwrap();
    assert!(gitlab
        .file_url("group/repo", "main", "a.py", None, None)
        .contains("/-/blob/main/a.py"));
}
#[test]
fn merge_is_bound_to_reviewed_head_and_method() {
    let request = Operation::Merge {
        number: 7,
        sha: "reviewed-head".into(),
        method: "squash".into(),
        title: "Title".into(),
        message: "Body".into(),
    }
    .plan("owner/repo")
    .unwrap();
    assert_eq!(request.method, "PUT");
    assert_eq!(request.path, "/repos/owner/repo/pulls/7/merge");
    assert_eq!(request.body.unwrap()["sha"], "reviewed-head");
    assert!(Operation::InlineComment {
        number: 7,
        sha: "head".into(),
        reply_to: None,
        comment: InlineComment {
            path: "../secret".into(),
            line: 1,
            side: "RIGHT".into(),
            body: "Comment".into()
        }
    }
    .plan("owner/repo")
    .is_err());
}
#[test]
fn issue_search_paginates_and_excludes_pull_requests() {
    let (client, requests) = recorded(
        Provider::GitHub,
        vec![
            (
                json!({"items":[{"number":1,"title":"Issue"},{"number":2,"pull_request":{}}]}),
                Some(2),
            ),
            (json!({"items":[{"number":3,"title":"Next"}]}), None),
        ],
    );
    let issues = client
        .issues("team/repo", "is:open label:bug", &CancellationToken::new())
        .unwrap();
    assert_eq!(
        issues.iter().map(|issue| issue.number).collect::<Vec<_>>(),
        vec![1, 3]
    );
    let sent = requests.lock().unwrap();
    assert!(sent[0].path.contains("is%3Aissue"));
    assert!(sent[1].path.ends_with("page=2"));
}
#[test]
fn gitlab_worklogs_use_selected_account_and_nested_repository() {
    let (client, requests) = recorded(
        Provider::GitLab,
        vec![(json!({"total_time_spent":60}), None)],
    );
    client
        .issue_spent_time(
            "group/nested/repo",
            12,
            60,
            "Investigation",
            &CancellationToken::new(),
        )
        .unwrap();
    let sent = requests.lock().unwrap();
    assert_eq!(
        sent[0].path,
        "/projects/group%2Fnested%2Frepo/issues/12/add_spent_time"
    );
    assert_eq!(sent[0].body.as_ref().unwrap()["duration"], "60s");
}
#[test]
fn diff_comments_keep_old_and_new_line_sides() {
    let lines = diff_lines("@@ -3,2 +3,3 @@\n-old\n+new\n+added\n context\n");
    assert_eq!((lines[0].old, lines[0].new), (Some(3), None));
    assert_eq!((lines[1].old, lines[1].new), (None, Some(3)));
    assert_eq!((lines[3].old, lines[3].new), (Some(4), Some(5)));
}

#[test]
fn gitlab_list_without_diff_refs_retains_merge_request_identity() {
    let (client, _) = recorded(
        Provider::GitLab,
        vec![(
            json!([{"iid":19,"id":90,"title":"Draft: Improve","source_branch":"feature","target_branch":"main","sha":"abc","state":"opened","author":{"id":2,"username":"alice"},"labels":[]}]),
            None,
        )],
    );
    let pulls = client
        .pulls(
            "group/repo",
            &Search {
                state: "open".into(),
                ..Default::default()
            },
            &CancellationToken::new(),
        )
        .unwrap();
    assert_eq!(pulls[0].number, 19);
    assert_eq!(pulls[0].head.sha, "abc");
    assert_eq!(pulls[0].user.login, "alice");
    assert_eq!(pulls[0].id, 90);
    assert_eq!(pulls[0].head.name, "feature");
    assert_eq!(pulls[0].base.name, "main");
    assert!(pulls[0].html_url.is_empty());
    assert!(pulls[0].draft);
}

#[test]
fn gitlab_nullable_optional_metadata_does_not_erase_merge_request_identity() {
    let (client, _) = recorded(
        Provider::GitLab,
        vec![(
            json!([{
                "iid": 21, "id": 91, "title": "Ready", "source_branch": "feature", "target_branch": "main",
                "sha": "head-sha", "state": "opened", "web_url": null, "updated_at": null,
                "author": {"id": 2, "username": "alice", "avatar_url": null},
                "reviewers": [{"id": 3, "username": "reviewer", "avatar_url": null}],
                "labels": ["bug"], "description": null, "diff_refs": null
            }]),
            None,
        )],
    );
    let pulls = client
        .pulls("group/repo", &Search::default(), &CancellationToken::new())
        .unwrap();
    assert_eq!(pulls[0].number, 21);
    assert_eq!(pulls[0].head.sha, "head-sha");
    assert_eq!(pulls[0].user.login, "alice");
    assert_eq!(pulls[0].requested_reviewers[0].login, "reviewer");
    assert_eq!(pulls[0].labels[0].name, "bug");
    assert!(pulls[0].body.is_none());
    assert!(pulls[0].html_url.is_empty());
}

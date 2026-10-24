//! Optional GitLab REST adapter for the shared review workspace.
use super::*;
use rustpy_core::CancellationToken;
use serde_json::{json, Value};
fn project(repository: &str) -> Result<String, String> {
    repository_path(repository)?;
    Ok(format!("/projects/{}", encode(repository)))
}
fn user(value: &Value) -> Value {
    let mut user = if value.is_object() {
        value.clone()
    } else {
        json!({})
    };
    user["login"] = value["username"].clone();
    if let Some(object) = user.as_object_mut() {
        object.remove("username");
    }
    user
}
fn repository(value: Value) -> Repository {
    Repository {
        id: value["id"].as_u64().unwrap_or_default(),
        name: value["name"].as_str().unwrap_or_default().into(),
        full_name: value["path_with_namespace"]
            .as_str()
            .unwrap_or_default()
            .into(),
        description: value["description"].as_str().map(str::to_owned),
        private: value["visibility"] != "public",
        html_url: value["web_url"].as_str().unwrap_or_default().into(),
        clone_url: value["http_url_to_repo"]
            .as_str()
            .unwrap_or_default()
            .into(),
        ssh_url: value["ssh_url_to_repo"].as_str().unwrap_or_default().into(),
        default_branch: value["default_branch"].as_str().unwrap_or("main").into(),
        owner: serde_json::from_value(user(&value["owner"])).unwrap_or_default(),
    }
}
fn pull(value: Value) -> PullRequest {
    fn parsed_user(value: &Value) -> User {
        User {
            id: value["id"].as_u64().unwrap_or_default(),
            login: value["username"].as_str().unwrap_or_default().into(),
            name: value["name"].as_str().map(str::to_owned),
            avatar_url: value["avatar_url"].as_str().unwrap_or_default().into(),
        }
    }
    let users = |key: &str| {
        value[key]
            .as_array()
            .map(|users| users.iter().map(parsed_user).collect())
            .unwrap_or_default()
    };
    let text = |key: &str| value[key].as_str().unwrap_or_default().to_owned();
    // GitLab list responses can omit optional detail fields. A null web_url
    // must not make deserialization discard the MR's iid, branches and author.
    PullRequest {
        id: value["id"].as_u64().unwrap_or_default(),
        node_id: text("node_id"),
        number: value["iid"].as_u64().unwrap_or_default(),
        title: text("title"),
        body: value["description"].as_str().map(str::to_owned),
        state: if value["state"] == "opened" {
            "open".into()
        } else {
            text("state")
        },
        draft: value["draft"].as_bool().unwrap_or(false)
            || value["work_in_progress"].as_bool().unwrap_or(false)
            || text("title").starts_with("Draft:")
            || text("title").starts_with("WIP:"),
        html_url: text("web_url"),
        user: parsed_user(&value["author"]),
        head: Ref {
            name: text("source_branch"),
            sha: text("sha"),
            ..Default::default()
        },
        base: Ref {
            name: text("target_branch"),
            sha: value["diff_refs"]["base_sha"]
                .as_str()
                .unwrap_or_default()
                .into(),
            ..Default::default()
        },
        labels: value["labels"]
            .as_array()
            .map(|labels| {
                labels
                    .iter()
                    .map(|label| Label {
                        name: label
                            .as_str()
                            .or_else(|| label["name"].as_str())
                            .unwrap_or_default()
                            .into(),
                        color: label["color"].as_str().unwrap_or_default().into(),
                    })
                    .collect()
            })
            .unwrap_or_default(),
        assignees: users("assignees"),
        requested_reviewers: users("reviewers"),
        mergeable: value["mergeable"].as_bool(),
        mergeable_state: value["detailed_merge_status"]
            .as_str()
            .or_else(|| value["merge_status"].as_str())
            .unwrap_or_default()
            .into(),
        merged: value["state"] == "merged",
        updated_at: text("updated_at"),
        diff_refs: value["diff_refs"].clone(),
    }
}
pub fn repositories<T: Transport>(
    client: &Client<T>,
    organization: &str,
    search: &str,
    cancel: &CancellationToken,
) -> Result<Vec<Repository>, String> {
    let path = if organization.is_empty() {
        format!(
            "/projects?membership=true&order_by=last_activity_at&search={}",
            encode(search)
        )
    } else {
        format!(
            "/groups/{}/projects?include_subgroups=true&search={}",
            encode(organization),
            encode(search)
        )
    };
    Ok(client
        .pages(&path, None, cancel)?
        .into_iter()
        .map(repository)
        .collect())
}
pub fn pulls<T: Transport>(
    client: &Client<T>,
    repository: &str,
    search: &Search,
    cancel: &CancellationToken,
) -> Result<Vec<PullRequest>, String> {
    let state = match search.state.as_str() {
        "open" => "opened",
        "closed" => "closed",
        "merged" => "merged",
        _ => "all",
    };
    let mut path = format!(
        "{}/merge_requests?scope=all&state={state}&search={}",
        project(repository)?,
        encode(&search.text)
    );
    if !search.author.is_empty() {
        path.push_str(&format!("&author_username={}", encode(&search.author)));
    }
    if !search.assignee.is_empty() {
        path.push_str(&format!("&assignee_username={}", encode(&search.assignee)));
    }
    if !search.label.is_empty() {
        path.push_str(&format!("&labels={}", encode(&search.label)));
    }
    if !search.review.is_empty() {
        path.push_str(&format!("&reviewer_username={}", encode(&search.review)));
    }
    Ok(client
        .pages(&path, None, cancel)?
        .into_iter()
        .map(pull)
        .collect())
}
pub fn details<T: Transport>(
    client: &Client<T>,
    repository: &str,
    number: u64,
    cancel: &CancellationToken,
) -> Result<PullDetails, String> {
    let prefix = format!("{}/merge_requests/{number}", project(repository)?);
    let detail = client.send(&Request::read(&prefix), cancel)?.value;
    let mut result = PullDetails {
        pull: pull(detail),
        ..Default::default()
    };
    for value in client.pages(&format!("{prefix}/diffs"), None, cancel)? {
        result.files.push(PullFile {
            filename: value["new_path"].as_str().unwrap_or_default().into(),
            previous_filename: value["old_path"].as_str().map(str::to_owned),
            status: if value["new_file"] == true {
                "added"
            } else if value["deleted_file"] == true {
                "removed"
            } else if value["renamed_file"] == true {
                "renamed"
            } else {
                "modified"
            }
            .into(),
            patch: value["diff"].as_str().map(str::to_owned),
            ..Default::default()
        });
    }
    for value in client.pages(&format!("{prefix}/commits"), None, cancel)? {
        result.commits.push(Commit {
            sha: value["id"].as_str().unwrap_or_default().into(),
            html_url: value["web_url"].as_str().unwrap_or_default().into(),
            commit: CommitData {
                message: value["message"].as_str().unwrap_or_default().into(),
                author: CommitAuthor {
                    name: value["author_name"].as_str().unwrap_or_default().into(),
                    date: value["created_at"].as_str().unwrap_or_default().into(),
                },
            },
            author: None,
        });
    }
    for discussion in client.pages(&format!("{prefix}/discussions"), None, cancel)? {
        for note in discussion["notes"].as_array().into_iter().flatten() {
            let mut normalized = note.clone();
            normalized["user"] = user(&note["author"]);
            normalized["path"] = json!(note["position"]["new_path"].as_str().unwrap_or_default());
            normalized["line"] = note["position"]["new_line"].clone();
            normalized["original_line"] = note["position"]["old_line"].clone();
            normalized["side"] = json!(if note["position"]["new_line"].is_null() {
                "LEFT"
            } else {
                "RIGHT"
            });
            normalized["commit_id"] = note["position"]["head_sha"].clone();
            if let Ok(comment) = serde_json::from_value(normalized) {
                if note["position"].is_object() {
                    result.comments.push(comment);
                } else {
                    result.issue_comments.push(comment);
                }
            }
        }
    }
    if let Ok(response) = client.send(&Request::read(format!("{prefix}/approvals")), cancel) {
        for approval in response.value["approved_by"]
            .as_array()
            .into_iter()
            .flatten()
        {
            result.reviews.push(Review {
                state: "APPROVED".into(),
                user: serde_json::from_value(user(&approval["user"])).unwrap_or_default(),
                ..Default::default()
            });
        }
    }
    if let Ok(drafts) = client.pages(&format!("{prefix}/draft_notes"), None, cancel) {
        if !drafts.is_empty() {
            if let Ok(current) = client.send(&Request::read("/user"), cancel) {
                result.reviews.push(Review {
                    id: 1,
                    state: "PENDING".into(),
                    user: serde_json::from_value(user(&current.value)).unwrap_or_default(),
                    body: format!("{} pending draft notes", drafts.len()),
                    ..Default::default()
                });
            }
        }
    }
    if let Ok(values) = client.pages(&format!("{prefix}/pipelines"), None, cancel) {
        for pipeline in values {
            result.checks.push(Check {
                id: pipeline["id"].as_u64().unwrap_or_default(),
                name: format!("Pipeline {}", pipeline["id"]),
                status: pipeline["status"].as_str().unwrap_or_default().into(),
                conclusion: pipeline["status"].as_str().map(str::to_owned),
                html_url: pipeline["web_url"].as_str().unwrap_or_default().into(),
                details_url: None,
            });
        }
    }
    Ok(result)
}
fn ids<T: Transport>(
    client: &Client<T>,
    names: &[String],
    cancel: &CancellationToken,
) -> Result<Vec<u64>, String> {
    let mut ids = Vec::new();
    for name in names {
        let response = client.send(
            &Request::read(format!("/users?username={}", encode(name))),
            cancel,
        )?;
        let id = response
            .value
            .as_array()
            .and_then(|users| users.first())
            .and_then(|user| user["id"].as_u64())
            .ok_or_else(|| format!("GitLab user not found: {name}"))?;
        ids.push(id);
    }
    Ok(ids)
}
fn position<T: Transport>(
    client: &Client<T>,
    prefix: &str,
    comment: &InlineComment,
    sha: &str,
    cancel: &CancellationToken,
) -> Result<Value, String> {
    comment.validate()?;
    let current = client.send(&Request::read(prefix), cancel)?.value;
    if current["sha"].as_str() != Some(sha) {
        return Err("Merge-request head changed; refresh before commenting".into());
    }
    let diffs = client.pages(&format!("{prefix}/diffs"), None, cancel)?;
    let file = diffs
        .iter()
        .find(|file| {
            file["new_path"].as_str() == Some(&comment.path)
                || file["old_path"].as_str() == Some(&comment.path)
        })
        .ok_or("The commented file is absent from the current diff")?;
    let old_path = file["old_path"].as_str().ok_or("Diff has no old path")?;
    let new_path = file["new_path"].as_str().ok_or("Diff has no new path")?;
    let mut position = json!({"position_type":"text","base_sha":current["diff_refs"]["base_sha"],"head_sha":sha,"start_sha":current["diff_refs"]["start_sha"],"new_path":new_path,"old_path":old_path});
    position[if comment.side == "LEFT" {
        "old_line"
    } else {
        "new_line"
    }] = json!(comment.line);
    Ok(position)
}
pub fn execute<T: Transport>(
    client: &Client<T>,
    repository: &str,
    operation: &Operation,
    cancel: &CancellationToken,
) -> Result<Value, String> {
    let base = if matches!(
        operation,
        Operation::CreateRepository { .. } | Operation::CreateGist { .. }
    ) {
        String::new()
    } else {
        project(repository)?
    };
    let request = match operation {
        Operation::CreateRepository {
            name,
            description,
            private,
            organization,
        } => {
            let mut body = json!({"name":name,"description":description,"visibility":if *private{"private"}else{"public"},"initialize_with_readme":false});
            if !organization.is_empty() {
                let group = client
                    .send(
                        &Request::read(format!("/groups/{}", encode(organization))),
                        cancel,
                    )?
                    .value;
                body["namespace_id"] = group["id"].clone();
            }
            Request::write("POST", "/projects", body)
        }
        Operation::CreateGist {
            description,
            public,
            files,
        } => Request::write(
            "POST",
            "/snippets",
            json!({"title":if description.is_empty(){"Snippet"}else{description},"description":description,"visibility":if *public{"public"}else{"private"},"files":files.iter().map(|(name,text)|json!({"file_path":name,"content":text})).collect::<Vec<_>>()}),
        ),
        Operation::CreatePull(pull) => Request::write(
            "POST",
            format!("{base}/merge_requests"),
            json!({"source_branch":pull.head,"target_branch":pull.base,"title":if pull.draft&&!pull.title.starts_with("Draft:"){format!("Draft: {}",pull.title)}else{pull.title.clone()},"description":pull.body,"reviewer_ids":ids(client,&pull.reviewers,cancel)?,"assignee_ids":ids(client,&pull.assignees,cancel)?,"labels":pull.labels.join(",")}),
        ),
        Operation::Comment { number, body } => Request::write(
            "POST",
            format!("{base}/merge_requests/{number}/notes"),
            json!({"body":body}),
        ),
        Operation::Review {
            number,
            sha,
            body,
            event,
            comments,
        } => {
            if event == "REQUEST_CHANGES" {
                return Err("GitLab change-request blocking is managed on the server; use Comment or Approve here".into());
            }
            let prefix = format!("{base}/merge_requests/{number}");
            for comment in comments {
                let position = position(client, &prefix, comment, sha, cancel)?;
                let path = if event.is_empty() {
                    format!("{prefix}/draft_notes")
                } else {
                    format!("{prefix}/discussions")
                };
                client.send(
                    &Request::write(
                        "POST",
                        path,
                        if event.is_empty() {
                            json!({"note":comment.body,"position":position})
                        } else {
                            json!({"body":comment.body,"position":position})
                        },
                    ),
                    cancel,
                )?;
            }
            if !body.trim().is_empty() {
                client.send(
                    &Request::write(
                        "POST",
                        if event.is_empty() {
                            format!("{prefix}/draft_notes")
                        } else {
                            format!("{prefix}/notes")
                        },
                        if event.is_empty() {
                            json!({"note":body})
                        } else {
                            json!({"body":body})
                        },
                    ),
                    cancel,
                )?;
            }
            if event == "APPROVE" {
                Request::write("POST", format!("{prefix}/approve"), json!({"sha":sha}))
            } else if event == "REQUEST_CHANGES" {
                return Err("GitLab change-request blocking is managed on the server; use Comment or Approve here".into());
            } else {
                return Ok(json!({"id":1,"state":if event.is_empty(){"PENDING"}else{"COMMENTED"}}));
            }
        }
        Operation::SubmitReview {
            number,
            event,
            body,
            ..
        } => {
            if event == "REQUEST_CHANGES" {
                return Err("GitLab change-request blocking is managed on the server; use Comment or Approve here".into());
            }
            let prefix = format!("{base}/merge_requests/{number}");
            client.send(
                &Request::write(
                    "POST",
                    format!("{prefix}/draft_notes/bulk_publish"),
                    json!({}),
                ),
                cancel,
            )?;
            if !body.is_empty() {
                client.send(
                    &Request::write("POST", format!("{prefix}/notes"), json!({"body":body})),
                    cancel,
                )?;
            }
            if event == "APPROVE" {
                Request::write("POST", format!("{prefix}/approve"), json!({}))
            } else {
                return Ok(json!({"state":"COMMENTED"}));
            }
        }
        Operation::InlineComment {
            number,
            comment,
            sha,
            reply_to,
        } => {
            let prefix = format!("{base}/merge_requests/{number}");
            if let Some(reply) = reply_to {
                let discussions = client.pages(&format!("{prefix}/discussions"), None, cancel)?;
                let discussion = discussions
                    .iter()
                    .find(|discussion| {
                        discussion["notes"].as_array().is_some_and(|notes| {
                            notes.iter().any(|note| note["id"].as_u64() == Some(*reply))
                        })
                    })
                    .and_then(|discussion| discussion["id"].as_str())
                    .ok_or("Discussion no longer exists")?;
                Request::write(
                    "POST",
                    format!("{prefix}/discussions/{}/notes", encode(discussion)),
                    json!({"body":comment.body}),
                )
            } else {
                Request::write(
                    "POST",
                    format!("{prefix}/discussions"),
                    json!({"body":comment.body,"position":position(client,&prefix,comment,sha,cancel)?}),
                )
            }
        }
        Operation::Merge {
            number,
            sha,
            method,
            message,
            ..
        } => {
            if method == "rebase" {
                return Err(
                    "Choose Merge or Squash for GitLab; configure rebase on the server".into(),
                );
            }
            Request::write(
                "PUT",
                format!("{base}/merge_requests/{number}/merge"),
                json!({"sha":sha,"squash":method=="squash","merge_commit_message":message}),
            )
        }
        Operation::Reviewers { number, reviewers } => Request::write(
            "PUT",
            format!("{base}/merge_requests/{number}"),
            json!({"reviewer_ids":ids(client,reviewers,cancel)?}),
        ),
        Operation::Metadata {
            number,
            assignees,
            labels,
        } => Request::write(
            "PUT",
            format!("{base}/merge_requests/{number}"),
            json!({"assignee_ids":ids(client,assignees,cancel)?,"labels":labels.join(",")}),
        ),
    };
    let mut value = client.send(&request, cancel)?.value;
    if let Operation::CreatePull(_) = operation {
        value = serde_json::to_value(pull(value)).map_err(|error| error.to_string())?;
    }
    if let Operation::CreateRepository { .. } = operation {
        value = serde_json::to_value(self::repository(value)).map_err(|error| error.to_string())?;
    }
    Ok(value)
}

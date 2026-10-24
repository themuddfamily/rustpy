//! Authenticated task adapters. Only explicit UI search/worklog actions call transport.
use base64::Engine;
use rustpy_core::CancellationToken;
use rustpy_settings::tasks_workspace::{Tracker, TrackerKind as K, WorkItem};
use serde_json::{json, Value};
use std::io::Read;

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Issue {
    pub id: String,
    pub remote_id: String,
    pub summary: String,
    pub url: String,
    pub tracker: String,
}
#[derive(Clone, Debug)]
pub(crate) struct Request {
    pub method: &'static str,
    pub path: String,
    pub body: Option<Value>,
}
fn get(path: String) -> Request {
    Request {
        method: "GET",
        path,
        body: None,
    }
}
fn post(path: String, body: Value) -> Request {
    Request {
        method: "POST",
        path,
        body: Some(body),
    }
}
pub(crate) fn encode(value: &str) -> String {
    value
        .bytes()
        .map(|byte| {
            if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
                (byte as char).to_string()
            } else {
                format!("%{byte:02X}")
            }
        })
        .collect()
}
fn text(value: &Value) -> Option<String> {
    value
        .as_str()
        .map(str::to_owned)
        .or_else(|| value.as_u64().map(|n| n.to_string()))
}
fn server_url(server: &Tracker, path: &str) -> Result<url::Url, String> {
    let base = url::Url::parse(&format!("{}/", server.url.trim_end_matches('/')))
        .map_err(|_| "Invalid tracker URL")?;
    if base.scheme() != "https"
        && !(base.scheme() == "http"
            && matches!(base.host_str(), Some("localhost" | "127.0.0.1" | "[::1]")))
    {
        return Err(
            "Tracker URL requires HTTPS (HTTP is allowed for loopback development servers)".into(),
        );
    }
    if !base.username().is_empty()
        || base.password().is_some()
        || base.query().is_some()
        || base.fragment().is_some()
    {
        return Err("Tracker URL must not contain credentials, query or fragment".into());
    }
    let url = base
        .join(path.trim_start_matches('/'))
        .map_err(|_| "Invalid tracker request path")?;
    if url.origin() != base.origin() || !url.path().starts_with(base.path()) {
        return Err("Tracker request escapes configured server".into());
    }
    Ok(url)
}
/// Search plans use native server filters; parsers perform a bounded title/id filter where the API has none.
pub(crate) fn search_request(s: &Tracker, q: &str) -> Result<Request, String> {
    let query = if s.search.is_empty() {
        q.to_owned()
    } else {
        s.search.replace("${query}", q)
    };
    let e = encode(&query);
    let p = encode(&s.project);
    Ok(match s.kind {
        K::YouTrack=>get(format!("api/issues?fields=id,idReadable,summary&$top=100&query={e}")),
        K::Jira=>get(format!("rest/api/3/search/jql?jql={}&fields=summary&maxResults=100",encode(&if s.search.is_empty(){if q.is_empty(){"assignee = currentUser() AND statusCategory != Done ORDER BY updated DESC".into()}else{format!("text ~ {} ORDER BY updated DESC",serde_json::to_string(q).unwrap())}}else{query}))),
        K::GitHub|K::GitLab=>return Err("Use the shared authenticated GitHub/GitLab account client".into()),
        K::Bugzilla=>get(format!("rest/bug?summary={e}&limit=100&include_fields=id,summary")),
        K::Trello=>get(format!("1/search?query={e}&modelTypes=cards&cards_limit=100&card_fields=name,url,idShort")),
        K::Redmine=>get(format!("issues.json?subject={}&limit=100&project_id={p}",encode(&format!("~{query}")))),
        K::Mantis=>get(format!("api/rest/issues?page_size=100&page=1&project_id={p}")),
        K::Lighthouse=>get(format!("projects/{p}/tickets.json?q={e}")),
        K::Pivotal=>get(format!("services/v5/projects/{p}/stories?filter={e}&limit=100")),
        K::Asana=>get(format!("api/1.0/workspaces/{p}/tasks/search?text={e}&limit=100&opt_fields=gid,name,permalink_url")),
        K::Trac=>post("login/jsonrpc".into(),json!({"method":"ticket.query","params":[if query.is_empty(){"status!=closed&max=100".into()}else{format!("summary~={query}&max=100")}],"id":1})),
        K::FogBugz=>post("f/api/0/jsonapi".into(),json!({"cmd":"search","q":query,"cols":["sTitle"],"max":100})),
        K::Generic=>get(s.generic.search_path.replace("${query}",&e).replace("${project}",&p)),
    })
}
fn perform(
    s: &Tracker,
    request: Request,
    secret: &str,
    cancel: &CancellationToken,
) -> Result<Value, String> {
    cancel.check().map_err(|e| e.to_string())?;
    if secret.contains(['\r', '\n']) {
        return Err("Credential contains a line separator".into());
    }
    let mut url = server_url(s, &request.path)?;
    if s.kind == K::Trello {
        url.query_pairs_mut()
            .append_pair("key", &s.api_key)
            .append_pair("token", secret);
    }
    let agent = rustpy_net::agent_builder(url.as_str())
        .timeout_connect(std::time::Duration::from_secs(5))
        .timeout(std::time::Duration::from_secs(20))
        .redirects(0)
        .build();
    let mut req = agent
        .request(request.method, url.as_str())
        .set("Accept", "application/json")
        .set("Content-Type", "application/json")
        .set("User-Agent", "RustPy Tasks");
    let mut body = request.body;
    if !secret.is_empty() {
        req = match s.kind {
            K::Jira | K::Trac if !s.username.is_empty() => req.set(
                "Authorization",
                &format!(
                    "Basic {}",
                    base64::engine::general_purpose::STANDARD
                        .encode(format!("{}:{secret}", s.username))
                ),
            ),
            K::Bugzilla => req.set("X-BUGZILLA-API-KEY", secret),
            K::Redmine => req.set("X-Redmine-API-Key", secret),
            K::Lighthouse => req.set("X-LighthouseToken", secret),
            K::Pivotal => req.set("X-TrackerToken", secret),
            K::Trello => req,
            K::FogBugz => {
                if let Some(body) = body.as_mut() {
                    body["token"] = secret.into();
                }
                req
            }
            K::Generic => {
                if s.generic.auth_header.is_empty()
                    || s.generic.auth_header.contains(['\r', '\n'])
                    || s.generic.auth_prefix.contains(['\r', '\n'])
                {
                    return Err("Invalid authentication header".into());
                }
                req.set(
                    &s.generic.auth_header,
                    &format!("{}{secret}", s.generic.auth_prefix),
                )
            }
            _ => req.set("Authorization", &format!("Bearer {secret}")),
        };
    }
    let response = if let Some(body) = body {
        req.set("Content-Type", "application/json")
            .send_string(&body.to_string())
    } else {
        req.call()
    };
    let response = match response {
        Ok(response) => response,
        Err(ureq::Error::Status(code, _)) => {
            return Err(format!("{} returned HTTP {code}", s.kind.label()))
        }
        Err(ureq::Error::Transport(_)) => {
            return Err(format!(
                "{} request failed (connection, proxy or TLS); credentials and URLs omitted",
                s.kind.label()
            ))
        }
    };
    cancel.check().map_err(|e| e.to_string())?;
    let mut bytes = Vec::new();
    response
        .into_reader()
        .take(4 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "Cannot read tracker response")?;
    if bytes.len() > 4 * 1024 * 1024 {
        return Err("Tracker response exceeds 4 MiB".into());
    }
    if bytes.is_empty() {
        return Ok(Value::Null);
    }
    serde_json::from_slice(&bytes).map_err(|_| "Tracker returned invalid JSON".into())
}
fn secret(s: &Tracker) -> Result<String, String> {
    rustpy_secrets::get(&s.secret_key())
        .map_err(|e| e.to_string())
        .map(|value| value.unwrap_or_default())
}

pub(crate) fn search(
    s: &Tracker,
    query: &str,
    accounts: &rustpy_settings::github::GitHubSettings,
    cancel: &CancellationToken,
) -> Result<Vec<Issue>, String> {
    if matches!(s.kind, K::GitHub | K::GitLab) {
        let account = accounts
            .accounts
            .iter()
            .find(|account| Some(&account.id) == s.account_id.as_ref())
            .ok_or("Select an authenticated GitHub/GitLab account in Configure Servers")?;
        let expected = if s.kind == K::GitHub {
            rustpy_settings::github::Provider::GitHub
        } else {
            rustpy_settings::github::Provider::GitLab
        };
        if account.provider != expected {
            return Err("Tracker kind differs from selected account provider".into());
        }
        let client = rustpy_vcs::github::Client::account(account)?;
        let effective = if s.search.is_empty() {
            query.to_owned()
        } else {
            s.search.replace("${query}", query)
        };
        return client.issues(&s.project, &effective, cancel).map(|issues| {
            issues
                .into_iter()
                .filter(|issue| {
                    issue.title.len() <= 16384
                        && matches!(
                            url::Url::parse(&issue.html_url)
                                .ok()
                                .map(|url| url.scheme().to_owned())
                                .as_deref(),
                            Some("http" | "https")
                        )
                })
                .take(100)
                .map(|issue| Issue {
                    id: format!("#{}", issue.number),
                    remote_id: issue.number.to_string(),
                    summary: issue.title,
                    url: issue.html_url,
                    tracker: s.id.clone(),
                })
                .collect()
        });
    }
    let secret = secret(s)?;
    let request = search_request(s, query)?;
    let value = perform(s, request, &secret, cancel)?;
    if s.kind == K::Trac {
        if !value["error"].is_null() {
            return Err(
                "Trac JSON-RPC search failed; enable XmlRpcPlugin JSON-RPC and ticket permissions"
                    .into(),
            );
        }
        let ids = value["result"]
            .as_array()
            .ok_or("Trac query did not return ticket IDs")?;
        let mut issues = Vec::new();
        for id in ids.iter().take(100) {
            cancel.check().map_err(|e| e.to_string())?;
            let item = perform(
                s,
                post(
                    "login/jsonrpc".into(),
                    json!({"method":"ticket.get","params":[id],"id":2}),
                ),
                &secret,
                cancel,
            )?;
            if let Some(summary) = item["result"][3]["summary"].as_str() {
                let id = text(id).unwrap_or_default();
                issues.push(Issue {
                    id: format!("#{id}"),
                    remote_id: id.clone(),
                    summary: summary.into(),
                    url: format!("{}/ticket/{id}", s.url.trim_end_matches('/')),
                    tracker: s.id.clone(),
                });
            }
        }
        return Ok(issues);
    }
    parse_issues(s, &value, query)
}
pub(crate) fn parse_issues(s: &Tracker, value: &Value, query: &str) -> Result<Vec<Issue>, String> {
    let items = match s.kind {
        K::YouTrack | K::Pivotal => value.as_array(),
        K::Jira | K::Redmine | K::Mantis => value["issues"].as_array(),
        K::Bugzilla => value["bugs"].as_array(),
        K::Trello => value["cards"].as_array(),
        K::Lighthouse => value["tickets"].as_array().or_else(|| value.as_array()),
        K::Asana => value["data"].as_array(),
        K::FogBugz => value["data"]["cases"].as_array(),
        K::Generic => value
            .pointer(&s.generic.items_pointer)
            .and_then(Value::as_array),
        _ => None,
    }
    .ok_or_else(|| {
        format!(
            "{} response does not contain its issue list",
            s.kind.label()
        )
    })?;
    let mut result = Vec::new();
    for raw in items.iter().take(100) {
        let v = if s.kind == K::Lighthouse {
            raw.get("ticket").unwrap_or(raw)
        } else {
            raw
        };
        let (id, remote, title, url) = match s.kind {
            K::YouTrack => (
                text(&v["idReadable"]),
                text(&v["id"]),
                text(&v["summary"]),
                None,
            ),
            K::Jira => (
                text(&v["key"]),
                text(&v["key"]),
                text(&v["fields"]["summary"]),
                None,
            ),
            K::Bugzilla | K::Mantis => (text(&v["id"]), text(&v["id"]), text(&v["summary"]), None),
            K::Redmine => (text(&v["id"]), text(&v["id"]), text(&v["subject"]), None),
            K::Trello => (
                text(&v["idShort"]).or_else(|| text(&v["id"])),
                text(&v["id"]),
                text(&v["name"]),
                text(&v["url"]),
            ),
            K::Lighthouse => (
                text(&v["number"]),
                text(&v["number"]),
                text(&v["title"]),
                text(&v["url"]),
            ),
            K::Pivotal => (
                text(&v["id"]),
                text(&v["id"]),
                text(&v["name"]),
                text(&v["url"]),
            ),
            K::Asana => (
                text(&v["gid"]),
                text(&v["gid"]),
                text(&v["name"]),
                text(&v["permalink_url"]),
            ),
            K::FogBugz => (
                text(&v["ixBug"]),
                text(&v["ixBug"]),
                text(&v["sTitle"]),
                None,
            ),
            K::Generic => (
                v.pointer(&s.generic.id_pointer).and_then(text),
                v.pointer(&s.generic.id_pointer).and_then(text),
                v.pointer(&s.generic.title_pointer).and_then(text),
                v.pointer(&s.generic.url_pointer).and_then(text),
            ),
            _ => continue,
        };
        let (Some(id), Some(remote_id), Some(summary)) = (id, remote, title) else {
            continue;
        };
        if id.len() > 256 || remote_id.len() > 256 || summary.len() > 16384 {
            continue;
        }
        if s.kind == K::Mantis
            && !query.is_empty()
            && !format!("{id} {summary}")
                .to_lowercase()
                .contains(&query.to_lowercase())
        {
            continue;
        }
        let browser = match s.kind {
            K::YouTrack => format!("issue/{}", encode(&id)),
            K::Jira => format!("browse/{}", encode(&id)),
            K::Bugzilla => format!("show_bug.cgi?id={}", encode(&id)),
            K::Mantis => format!("view.php?id={}", encode(&id)),
            K::Redmine => format!("issues/{}", encode(&id)),
            K::FogBugz => format!("default.asp?{}", encode(&id)),
            K::Lighthouse => format!("projects/{}/tickets/{}", encode(&s.project), encode(&id)),
            _ => String::new(),
        };
        let url = url.unwrap_or_else(|| {
            if s.kind == K::Generic {
                s.generic
                    .browser_pattern
                    .replace("${server}", s.url.trim_end_matches('/'))
                    .replace("${id}", &encode(&id))
            } else {
                format!("{}/{browser}", s.url.trim_end_matches('/'))
            }
        });
        if !matches!(
            url::Url::parse(&url)
                .ok()
                .map(|u| u.scheme().to_owned())
                .as_deref(),
            Some("https" | "http")
        ) {
            continue;
        }
        result.push(Issue {
            id,
            remote_id,
            summary,
            url,
            tracker: s.id.clone(),
        });
    }
    Ok(result)
}

fn expand_worklog_json(
    pattern: &str,
    id: &str,
    seconds: u64,
    description: &str,
    date: &str,
) -> String {
    let mut output = String::new();
    let mut rest = pattern;
    while let Some(start) = rest.find("${") {
        output.push_str(&rest[..start]);
        let Some(end) = rest[start..].find('}').map(|end| start + end + 1) else {
            output.push_str(&rest[start..]);
            return output;
        };
        let token = &rest[start..end];
        let value = match token {
            "${id}" => id.to_owned(),
            "${id_json}" => serde_json::to_string(id).unwrap(),
            "${seconds}" => seconds.to_string(),
            "${description_json}" => serde_json::to_string(description).unwrap(),
            "${date}" => date.to_owned(),
            _ => token.to_owned(),
        };
        output.push_str(&value);
        rest = &rest[end..];
    }
    output.push_str(rest);
    output
}

pub(crate) fn worklog_request(s: &Tracker, id: &str, item: &WorkItem) -> Result<Request, String> {
    if item.seconds == 0 || item.seconds > 366 * 24 * 3600 || item.description.len() > 16384 {
        return Err("Work item duration/description exceeds limits".into());
    }
    let encoded = encode(id);
    let date = chrono::DateTime::from_timestamp(item.started as i64, 0)
        .ok_or("Invalid work date")?
        .format("%Y-%m-%d")
        .to_string();
    Ok(match s.kind {
        K::YouTrack=>post(format!("api/issues/{encoded}/timeTracking/workItems?fields=id"),json!({"date":item.started.saturating_mul(1000),"duration":{"minutes":item.seconds.div_ceil(60)},"text":item.description})),
        K::Jira=>post(format!("rest/api/3/issue/{encoded}/worklog"),json!({"timeSpentSeconds":item.seconds,"comment":{"type":"doc","version":1,"content":[{"type":"paragraph","content":[{"type":"text","text":if item.description.is_empty(){"Work tracked in RustPy"}else{&item.description}}]}]}})),
        K::GitLab=>post(format!("projects/{}/issues/{encoded}/add_spent_time",encode(&s.project)),json!({"duration":format!("{}s",item.seconds),"summary":item.description})),
        K::Redmine=>post("time_entries.json".into(),json!({"time_entry":{"issue_id":id.parse::<u64>().map_err(|_|"Redmine requires numeric issue id")?,"hours":item.seconds as f64/3600.,"comments":item.description,"spent_on":date}})),
        K::Bugzilla=>Request{method:"PUT",path:format!("rest/bug/{encoded}"),body:Some(json!({"work_time":item.seconds as f64/3600.,"comment":{"body":if item.description.is_empty(){"Work tracked in RustPy"}else{&item.description}}}))},
        K::FogBugz => {
            let start=chrono::DateTime::from_timestamp(item.started as i64,0).ok_or("Invalid interval start")?;
            let end=chrono::DateTime::from_timestamp(item.started.saturating_add(item.seconds) as i64,0).ok_or("Invalid interval end")?;
            post("f/api/0/jsonapi".into(),json!({"cmd":"newInterval","ixBug":id.parse::<u64>().map_err(|_|"FogBugz requires numeric case id")?,"dtStart":start.to_rfc3339(),"dtEnd":end.to_rfc3339()}))
        },
        K::Asana=>post(format!("api/1.0/tasks/{encoded}/time_tracking_entries"),json!({"data":{"duration_minutes":item.seconds.div_ceil(60),"entered_on":date}})),
        _ if !s.generic.worklog_path.is_empty()=>{
            let body=expand_worklog_json(&s.generic.worklog_json,id,item.seconds,&item.description,&date);
            post(s.generic.worklog_path.replace("${id}",&encoded).replace("${project}",&encode(&s.project)),serde_json::from_str(&body).map_err(|_|"Invalid generic worklog JSON; use ${description_json} without surrounding quotes")?)
        },
        _=>return Err(format!("{} has no native time-entry adapter; configure an explicit REST worklog endpoint if your installation provides one",s.kind.label())),
    })
}
pub(crate) fn send_time(
    s: &Tracker,
    id: &str,
    item: &WorkItem,
    accounts: &rustpy_settings::github::GitHubSettings,
    cancel: &CancellationToken,
) -> Result<String, String> {
    let request = worklog_request(s, id, item)?;
    let value = if s.kind == K::GitLab {
        let account = accounts
            .accounts
            .iter()
            .find(|account| Some(&account.id) == s.account_id.as_ref())
            .ok_or("Select an authenticated GitLab account")?;
        if account.provider != rustpy_settings::github::Provider::GitLab {
            return Err("Worklog account is not GitLab".into());
        }
        let client = rustpy_vcs::github::Client::account(account)?;
        client.issue_spent_time(
            &s.project,
            id.parse::<u64>()
                .map_err(|_| "GitLab requires numeric issue IID")?,
            item.seconds,
            &item.description,
            cancel,
        )?
    } else {
        perform(s, request, &secret(s)?, cancel)?
    };
    if (!value["error"].is_null() && value["error"] != false)
        || value["errors"]
            .as_array()
            .is_some_and(|errors| !errors.is_empty())
        || value["ok"] == false
        || value["success"] == false
    {
        return Err("Tracker rejected the work item".into());
    }
    Ok(text(&value["id"])
        .or_else(|| text(&value["data"]["gid"]))
        .or_else(|| text(&value["time_entry"]["id"]))
        .unwrap_or_else(|| "Accepted by tracker".into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn tracker(kind: K) -> Tracker {
        Tracker {
            kind,
            id: "server-a".into(),
            url: "https://tracker.example/base".into(),
            project: "team/repo".into(),
            ..Default::default()
        }
    }
    #[test]
    fn search_paths_are_native_and_queries_are_encoded() {
        for kind in K::ALL
            .iter()
            .copied()
            .filter(|kind| !matches!(kind, K::GitHub | K::GitLab))
        {
            let request = search_request(&tracker(kind), "a & b/#1").unwrap();
            assert!(!request.path.contains("a & b"));
            assert!(server_url(&tracker(kind), &request.path).is_ok());
        }
        assert!(search_request(&tracker(K::Jira), "login")
            .unwrap()
            .path
            .contains("search/jql"));
        assert_eq!(
            search_request(&tracker(K::FogBugz), "login")
                .unwrap()
                .body
                .unwrap()["cmd"],
            "search"
        );
    }
    #[test]
    fn credentials_cannot_escape_origin_or_base_path() {
        let s = tracker(K::Generic);
        assert!(server_url(&s, "https://evil.example/x").is_err());
        assert!(server_url(&s, "../escape").is_err());
        assert!(server_url(&s, "/issues")
            .unwrap()
            .as_str()
            .contains("/base/issues"));
    }
    #[test]
    fn native_issue_shapes_preserve_remote_id() {
        let s = tracker(K::YouTrack);
        let issues = parse_issues(
            &s,
            &json!([{"id":"2-42","idReadable":"APP-42","summary":"Crash"}]),
            "",
        )
        .unwrap();
        assert_eq!(issues[0].id, "APP-42");
        assert_eq!(issues[0].remote_id, "2-42");
        let s = tracker(K::Lighthouse);
        assert_eq!(
            parse_issues(
                &s,
                &json!({"tickets":[{"ticket":{"number":8,"title":"Fix"}}]}),
                ""
            )
            .unwrap()[0]
                .id,
            "8"
        );
        let s = tracker(K::FogBugz);
        assert_eq!(
            parse_issues(
                &s,
                &json!({"data":{"cases":[{"ixBug":12,"sTitle":"Fix"}]}}),
                ""
            )
            .unwrap()[0]
                .id,
            "12"
        );
    }
    #[test]
    fn all_native_list_shapes_produce_owned_task_identity() {
        for (kind, value, expected) in [
            (
                K::Jira,
                json!({"issues":[{"key":"APP-7","fields":{"summary":"Title"}}]}),
                "APP-7",
            ),
            (
                K::Bugzilla,
                json!({"bugs":[{"id":7,"summary":"Title"}]}),
                "7",
            ),
            (
                K::Redmine,
                json!({"issues":[{"id":7,"subject":"Title"}]}),
                "7",
            ),
            (
                K::Mantis,
                json!({"issues":[{"id":7,"summary":"Title"}]}),
                "7",
            ),
            (
                K::Trello,
                json!({"cards":[{"id":"opaque","idShort":7,"name":"Title","url":"https://trello.com/c/opaque"}]}),
                "7",
            ),
            (
                K::Pivotal,
                json!([{"id":7,"name":"Title","url":"https://tracker.example/story/show/7"}]),
                "7",
            ),
            (
                K::Asana,
                json!({"data":[{"gid":"7","name":"Title","permalink_url":"https://app.asana.com/0/0/7"}]}),
                "7",
            ),
        ] {
            let issues = parse_issues(&tracker(kind), &value, "").unwrap();
            assert_eq!(issues.len(), 1, "{}", kind.label());
            assert_eq!(issues[0].id, expected);
            assert_eq!(issues[0].tracker, "server-a");
            assert_eq!(issues[0].summary, "Title");
        }
    }
    #[test]
    fn fogbugz_imports_the_actual_recorded_interval() {
        let item = WorkItem {
            started: 1700000000,
            seconds: 90,
            ..Default::default()
        };
        let body = worklog_request(&tracker(K::FogBugz), "42", &item)
            .unwrap()
            .body
            .unwrap();
        assert_eq!(body["cmd"], "newInterval");
        assert_eq!(body["ixBug"], 42);
        let start =
            chrono::DateTime::parse_from_rfc3339(body["dtStart"].as_str().unwrap()).unwrap();
        let end = chrono::DateTime::parse_from_rfc3339(body["dtEnd"].as_str().unwrap()).unwrap();
        assert_eq!((end - start).num_seconds(), 90);
    }
    #[test]
    fn generic_mapping_handles_nested_json_and_rejects_browser_commands() {
        let mut server = tracker(K::Generic);
        server.generic.items_pointer = "/results/items".into();
        server.generic.title_pointer = "/fields/summary".into();
        let values = json!({"results":{"items":[{"id":"A-1","fields":{"summary":"Nested"},"url":"javascript:alert(1)"},{"id":"A-2","fields":{"summary":"Visible"},"url":"https://tracker.example/A-2"}]}});
        let issues = parse_issues(&server, &values, "").unwrap();
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].id, "A-2");
    }
    #[test]
    fn worklog_placeholder_text_is_not_recursively_interpolated() {
        let rendered = expand_worklog_json(
            r#"{"id":${id_json},"description":${description_json},"seconds":${seconds},"date":"${date}"}"#,
            "${date}",
            12,
            "${seconds}",
            "2026-10-15",
        );
        let value: Value = serde_json::from_str(&rendered).unwrap();
        assert_eq!(value["id"], "${date}");
        assert_eq!(value["description"], "${seconds}");
        assert_eq!(value["seconds"], 12);
    }
    #[test]
    fn worklog_precision_and_json_escaping() {
        let item = WorkItem {
            seconds: 61,
            started: 1700000000,
            description: "quoted \"text\"\nline".into(),
            ..Default::default()
        };
        assert_eq!(
            worklog_request(&tracker(K::YouTrack), "x", &item)
                .unwrap()
                .body
                .unwrap()["duration"]["minutes"],
            2
        );
        let mut s = tracker(K::Generic);
        s.generic.worklog_path = "work/${id}".into();
        assert_eq!(
            worklog_request(&s, "42", &item).unwrap().body.unwrap()["description"],
            item.description
        );
        assert!(worklog_request(&tracker(K::GitHub), "42", &item).is_err());
    }
}

#[derive(Clone, Default)]
pub(crate) struct CommitTasks {
    pub suggestions: Vec<(String, String)>,
    pub known: Vec<(String, String)>,
    pub patterns: Vec<rustpy_settings::tasks_workspace::IssueNavigation>,
}
fn commit_data_id() -> egui::Id {
    egui::Id::new("rustpy-task-commit-authority")
}
pub(crate) fn set_commit_tasks(ctx: &egui::Context, tasks: CommitTasks) {
    ctx.data_mut(|data| data.insert_temp(commit_data_id(), tasks));
}
/// Suggestions complete the word at the actual TextEdit caret, including Unicode prefixes.
pub(crate) fn completion_range(message: &str, caret: usize) -> (usize, usize, &str) {
    let end = message
        .char_indices()
        .nth(caret)
        .map(|(byte, _)| byte)
        .unwrap_or(message.len());
    let start = message[..end]
        .char_indices()
        .rev()
        .find(|(_, c)| c.is_whitespace() || matches!(c, ',' | ';' | '(' | ')' | '[' | ']'))
        .map(|(byte, c)| byte + c.len_utf8())
        .unwrap_or(0);
    (start, end, &message[start..end])
}
pub(crate) fn commit_completion(
    ui: &mut egui::Ui,
    message: &mut String,
    response: &egui::Response,
) {
    if !response.has_focus() {
        return;
    }
    let Some(tasks) = ui
        .ctx()
        .data(|data| data.get_temp::<CommitTasks>(commit_data_id()))
    else {
        return;
    };
    let Some(mut state) = egui::TextEdit::load_state(ui.ctx(), response.id) else {
        return;
    };
    let Some(range) = state
        .cursor
        .char_range()
        .filter(|range| range.primary.index == range.secondary.index)
    else {
        return;
    };
    let (start, end, prefix) = completion_range(message, range.primary.index.0);
    let requested =
        ui.input_mut(|input| input.consume_key(egui::Modifiers::CTRL, egui::Key::Space));
    if !requested
        && (prefix.is_empty()
            || !tasks
                .suggestions
                .iter()
                .any(|(id, _)| id.to_lowercase().starts_with(&prefix.to_lowercase())))
    {
        return;
    }
    let candidates: Vec<_> = tasks
        .suggestions
        .iter()
        .filter(|(id, _)| id.to_lowercase().starts_with(&prefix.to_lowercase()))
        .take(8)
        .collect();
    let mut chosen = None;
    ui.horizontal_wrapped(|ui| {
        ui.label("Task IDs:");
        for (id, title) in candidates {
            if ui.small_button(format!("{id} {title}")).clicked() {
                chosen = Some(id.clone());
            }
        }
    });
    if let Some(id) = chosen {
        let index = message[..start].chars().count() + id.chars().count();
        message.replace_range(start..end, &id);
        state
            .cursor
            .set_char_range(Some(egui::text::CCursorRange::one(
                egui::text::CCursor::new(index),
            )));
        state.store(ui.ctx(), response.id);
        response.request_focus();
    }
}
pub(crate) fn issue_navigation(ui: &mut egui::Ui, message: &str) {
    let Some(tasks) = ui
        .ctx()
        .data(|data| data.get_temp::<CommitTasks>(commit_data_id()))
    else {
        return;
    };
    let mut links = std::collections::HashSet::new();
    for (id, url) in tasks.known {
        if message.contains(&id)
            && matches!(
                url::Url::parse(&url)
                    .ok()
                    .map(|url| url.scheme().to_owned())
                    .as_deref(),
                Some("https" | "http")
            )
            && links.insert(url.clone())
        {
            ui.hyperlink_to(id, url);
        }
    }
    for pattern in tasks.patterns.iter().take(32) {
        if pattern.pattern.len() > 4096 {
            continue;
        }
        if let Ok(regex) = regex::Regex::new(&pattern.pattern) {
            for captures in regex.captures_iter(message).take(20) {
                let mut url = String::new();
                captures.expand(&pattern.replacement, &mut url);
                if matches!(
                    url::Url::parse(&url)
                        .ok()
                        .map(|url| url.scheme().to_owned())
                        .as_deref(),
                    Some("https" | "http")
                ) && links.insert(url.clone())
                {
                    ui.hyperlink_to(captures.get(0).unwrap().as_str(), url);
                }
            }
        }
    }
}
#[cfg(test)]
mod completion_tests {
    use super::*;
    #[test]
    fn issue_word_completion_preserves_unicode_and_surrounding_message() {
        let message = "修复: (APP-1) next";
        let caret = "修复: (APP-1".chars().count();
        let (start, end, prefix) = completion_range(message, caret);
        assert_eq!(prefix, "APP-1");
        let mut changed = message.to_owned();
        changed.replace_range(start..end, "APP-123");
        assert_eq!(changed, "修复: (APP-123) next");
    }
}

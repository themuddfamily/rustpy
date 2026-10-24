//! Docker integration through the `docker` CLI (works with Docker Desktop,
//! Colima, remote `DOCKER_HOST`s and Podman's docker-compatible CLI):
//! containers, images and Compose projects, plus lifecycle commands.

use serde::Deserialize;
use std::time::Duration;
mod supervised;
pub use supervised::{Capture, SnapshotTask};

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct Container {
    #[serde(rename = "ID")]
    pub id: String,
    #[serde(rename = "Names")]
    pub names: String,
    #[serde(rename = "Image")]
    pub image: String,
    #[serde(rename = "State")]
    pub state: String,
    #[serde(rename = "Status")]
    pub status: String,
    #[serde(rename = "Ports")]
    pub ports: String,
    #[serde(rename = "Labels")]
    pub labels: String,
}

impl Container {
    pub fn is_running(&self) -> bool {
        self.state.eq_ignore_ascii_case("running")
    }

    /// Compose project from the `com.docker.compose.project` label.
    pub fn compose_project(&self) -> Option<&str> {
        self.labels
            .split(',')
            .find_map(|kv| kv.strip_prefix("com.docker.compose.project="))
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct Image {
    #[serde(rename = "ID")]
    pub id: String,
    #[serde(rename = "Repository")]
    pub repository: String,
    #[serde(rename = "Tag")]
    pub tag: String,
    #[serde(rename = "Size")]
    pub size: String,
    #[serde(rename = "CreatedSince")]
    pub created_since: String,
}

impl Image {
    pub fn reference(&self) -> String {
        if self.repository == "<none>" {
            self.id.clone()
        } else if self.tag.is_empty() || self.tag == "<none>" {
            self.repository.clone()
        } else {
            format!("{}:{}", self.repository, self.tag)
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct ComposeProject {
    #[serde(rename = "Name")]
    pub name: String,
    #[serde(rename = "Status")]
    pub status: String,
    #[serde(rename = "ConfigFiles")]
    pub config_files: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Snapshot {
    pub server_version: String,
    pub containers: Vec<Container>,
    pub images: Vec<Image>,
    pub compose: Vec<ComposeProject>,
    pub warnings: Vec<String>,
}

/// `docker ps --format '{{json .}}'` / `docker images --format '{{json .}}'`: one JSON object per line.
fn parse_lines<T: for<'de> Deserialize<'de>>(out: &str) -> Vec<T> {
    out.lines()
        .filter(|l| l.trim_start().starts_with('{'))
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

pub fn parse_containers(out: &str) -> Vec<Container> {
    parse_lines(out)
}

pub fn parse_images(out: &str) -> Vec<Image> {
    parse_lines(out)
}

/// `docker compose ls --format json` prints a JSON array.
pub fn parse_compose_ls(out: &str) -> Vec<ComposeProject> {
    serde_json::from_str(out.trim()).unwrap_or_else(|_| parse_lines(out))
}

/// Runs a captured command with a timeout. Timeout/cancellation cleans the
/// complete owned process tree and drains both bounded readers before returning.
pub fn run(program: &str, args: &[&str], timeout: Duration) -> anyhow::Result<String> {
    supervised::run_compat(program, args, timeout)
}

/// Collects containers, images and Compose projects.
pub fn snapshot(program: &str) -> anyhow::Result<Snapshot> {
    supervised::snapshot_compat(program)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContainerAction {
    Start,
    Stop,
    Restart,
    Remove,
}

/// Arguments for a container lifecycle command.
pub fn container_args(action: ContainerAction, id: &str) -> Vec<String> {
    let verb = match action {
        ContainerAction::Start => "start",
        ContainerAction::Stop => "stop",
        ContainerAction::Restart => "restart",
        ContainerAction::Remove => "rm",
    };
    let mut v = vec![verb.to_string()];
    if action == ContainerAction::Remove {
        v.push("-f".into());
    }
    v.push(id.to_string());
    v
}

/// Arguments for `docker compose -f <files…> <verb>` on a project.
pub fn compose_args(project: &ComposeProject, verb: &str) -> Vec<String> {
    let mut v = vec!["compose".to_string()];
    for f in project
        .config_files
        .split(',')
        .map(str::trim)
        .filter(|f| !f.is_empty())
    {
        v.push("-f".into());
        v.push(f.to_string());
    }
    v.push("-p".into());
    v.push(project.name.clone());
    v.extend(verb.split_whitespace().map(str::to_string));
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    const PS: &str = r#"{"Command":"\"python app.py\"","CreatedAt":"2026-10-01 10:00:00 +0000 UTC","ID":"3f2c1b","Image":"python:3.12","Labels":"com.docker.compose.project=shop,com.docker.compose.service=web","Names":"shop-web-1","Ports":"0.0.0.0:8000->8000/tcp","State":"running","Status":"Up 2 hours"}
{"Command":"\"redis-server\"","ID":"9a8b7c","Image":"redis:7","Labels":"","Names":"cache","Ports":"","State":"exited","Status":"Exited (0) 3 days ago"}
"#;

    #[test]
    fn parses_containers_with_compose_labels() {
        let cs = parse_containers(PS);
        assert_eq!(cs.len(), 2);
        assert!(cs[0].is_running() && !cs[1].is_running());
        assert_eq!(cs[0].compose_project(), Some("shop"));
        assert_eq!(cs[1].compose_project(), None);
        assert_eq!(cs[0].ports, "0.0.0.0:8000->8000/tcp");
    }

    #[test]
    fn parses_images_and_references() {
        let out = "{\"ID\":\"sha256:abc\",\"Repository\":\"python\",\"Tag\":\"3.12\",\"Size\":\"1.02GB\",\"CreatedSince\":\"2 weeks ago\"}\n{\"ID\":\"sha256:def\",\"Repository\":\"<none>\",\"Tag\":\"<none>\",\"Size\":\"5MB\",\"CreatedSince\":\"1 hour ago\"}\nWARNING: noise\n";
        let im = parse_images(out);
        assert_eq!(im.len(), 2);
        assert_eq!(im[0].reference(), "python:3.12");
        assert_eq!(im[1].reference(), "sha256:def");
    }

    #[test]
    fn parses_compose_ls_array_and_builds_args() {
        let ps = parse_compose_ls("[{\"Name\":\"shop\",\"Status\":\"running(2)\",\"ConfigFiles\":\"/srv/shop/compose.yaml,/srv/shop/compose.override.yaml\"}]");
        assert_eq!(ps.len(), 1);
        assert_eq!(
            compose_args(&ps[0], "up -d"),
            vec![
                "compose",
                "-f",
                "/srv/shop/compose.yaml",
                "-f",
                "/srv/shop/compose.override.yaml",
                "-p",
                "shop",
                "up",
                "-d"
            ]
        );
        assert_eq!(
            container_args(ContainerAction::Remove, "abc"),
            vec!["rm", "-f", "abc"]
        );
        assert_eq!(
            container_args(ContainerAction::Start, "abc"),
            vec!["start", "abc"]
        );
    }

    #[test]
    fn missing_program_is_an_error_not_a_panic() {
        let e = run(
            "definitely-not-a-docker-binary",
            &["ps"],
            Duration::from_secs(2),
        )
        .unwrap_err();
        assert!(e.to_string().contains("cannot run"));
    }

    #[test]
    fn timeouts_kill_the_child() {
        if Command::new("sleep").arg("0").status().is_err() {
            return;
        }
        let e = run("sleep", &["5"], Duration::from_millis(200)).unwrap_err();
        assert!(e.to_string().contains("timed out"));
    }
}

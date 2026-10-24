//! Captured Kubernetes CLI targets. Credential plugins remain owned by the configured
//! kubectl/Helm processes; no credentials are copied into project settings or logs.
use crate::CommandSpec;
use serde::{Deserialize, Serialize};
use std::path::Path;
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Target {
    pub kubectl: String,
    pub helm: String,
    pub kustomize: String,
    pub kubeconfig: String,
    pub context: String,
    pub namespace: String,
}
impl Default for Target {
    fn default() -> Self {
        Self {
            kubectl: "kubectl".into(),
            helm: "helm".into(),
            kustomize: String::new(),
            kubeconfig: String::new(),
            context: String::new(),
            namespace: "default".into(),
        }
    }
}
impl Target {
    pub fn load(root: &Path) -> Self {
        std::fs::read(root.join(".rustpy/kubernetes.json"))
            .ok()
            .and_then(|data| serde_json::from_slice(&data).ok())
            .unwrap_or_default()
    }
    pub fn save(&self, root: &Path) -> anyhow::Result<()> {
        self.validate()?;
        std::fs::create_dir_all(root.join(".rustpy"))?;
        let path = root.join(".rustpy/kubernetes.json");
        let staging = path.with_extension("json.new");
        std::fs::write(&staging, serde_json::to_vec_pretty(self)?)?;
        std::fs::rename(staging, path)?;
        Ok(())
    }
    pub fn validate(&self) -> anyhow::Result<()> {
        for field in [
            &self.kubectl,
            &self.helm,
            &self.kustomize,
            &self.kubeconfig,
            &self.context,
            &self.namespace,
        ] {
            anyhow::ensure!(
                !field.chars().any(char::is_control),
                "Kubernetes settings cannot contain control characters"
            );
        }
        anyhow::ensure!(
            !self.kubectl.trim().is_empty() && !self.helm.trim().is_empty(),
            "Configure kubectl and Helm executables"
        );
        Ok(())
    }
    pub fn kubectl(
        &self,
        args: impl IntoIterator<Item = impl Into<String>>,
    ) -> anyhow::Result<CommandSpec> {
        self.validate()?;
        let args = args.into_iter().map(Into::into).collect::<Vec<String>>();
        anyhow::ensure!(
            !self.context.is_empty()
                || matches!(
                    args.first().map(String::as_str),
                    Some("config" | "kustomize")
                ),
            "Select an explicit Kubernetes context before using cluster operations"
        );
        let mut command = CommandSpec::new(&self.kubectl);
        if !self.kubeconfig.is_empty() {
            command = command.arg(format!("--kubeconfig={}", self.kubeconfig));
        }
        if !self.context.is_empty() {
            command = command.arg(format!("--context={}", self.context));
        }
        if !self.namespace.is_empty() {
            command = command.arg(format!("--namespace={}", self.namespace));
        }
        Ok(command.arg("--request-timeout=30s").args(args))
    }
    pub fn helm(
        &self,
        args: impl IntoIterator<Item = impl Into<String>>,
    ) -> anyhow::Result<CommandSpec> {
        self.validate()?;
        anyhow::ensure!(
            !self.context.is_empty(),
            "Select an explicit Kubernetes context before Helm operations"
        );
        let mut command = CommandSpec::new(&self.helm);
        if !self.kubeconfig.is_empty() {
            command = command.arg(format!("--kubeconfig={}", self.kubeconfig));
        }
        if !self.context.is_empty() {
            command = command.arg(format!("--kube-context={}", self.context));
        }
        if !self.namespace.is_empty() {
            command = command.arg(format!("--namespace={}", self.namespace));
        }
        Ok(command.args(args))
    }
    pub fn browse(&self, kind: &str, all_namespaces: bool) -> anyhow::Result<CommandSpec> {
        resource_kind(kind)?;
        let mut args: Vec<String> = vec!["get".into(), kind.into(), "-o=json".into()];
        if all_namespaces {
            args.push("--all-namespaces".into());
        }
        self.kubectl(args)
    }
    pub fn contexts(&self) -> anyhow::Result<CommandSpec> {
        self.kubectl(["config", "get-contexts", "-o=name"])
    }
    pub fn logs(&self, pod: &str, container: &str, previous: bool) -> anyhow::Result<CommandSpec> {
        identifier(pod)?;
        let mut args = vec![
            "logs".into(),
            format!("pod/{pod}"),
            "--tail=1000".into(),
            "--timestamps=true".into(),
        ];
        if !container.is_empty() {
            identifier(container)?;
            args.push(format!("--container={container}"));
        }
        if previous {
            args.push("--previous=true".into());
        }
        self.kubectl(args)
    }
    pub fn resource(&self, operation: &str, kind: &str, name: &str) -> anyhow::Result<CommandSpec> {
        resource_kind(kind)?;
        identifier(name)?;
        anyhow::ensure!(
            matches!(operation, "describe" | "yaml" | "delete"),
            "Unsupported resource action"
        );
        if operation == "yaml" {
            self.kubectl(vec![
                "get".into(),
                format!("{kind}/{name}"),
                if kind == "secrets" {
                    "-o=json".into()
                } else {
                    "-o=yaml".into()
                },
            ])
        } else {
            self.kubectl(vec![operation.into(), format!("{kind}/{name}")])
        }
    }
    pub fn port_forward(
        &self,
        kind: &str,
        name: &str,
        local: u16,
        remote: u16,
    ) -> anyhow::Result<CommandSpec> {
        anyhow::ensure!(
            matches!(kind, "pods" | "services" | "deployments"),
            "Forward a pod, service or deployment"
        );
        identifier(name)?;
        anyhow::ensure!(
            local != 0 && remote != 0,
            "Ports must be between 1 and 65535"
        );
        let mut command = self.kubectl(vec![
            "port-forward".into(),
            "--address=127.0.0.1".into(),
            format!("{kind}/{name}"),
            format!("{local}:{remote}"),
        ])?;
        for argument in &mut command.args {
            if argument == "--request-timeout=30s" {
                *argument = "--request-timeout=0".into();
            }
        }
        Ok(command)
    }
    pub fn kustomize_plan(&self, directory: &Path) -> anyhow::Result<CommandSpec> {
        self.validate()?;
        if self.kustomize.is_empty() {
            self.kubectl(vec![
                "kustomize".into(),
                directory.to_string_lossy().into_owned(),
            ])
        } else {
            Ok(CommandSpec::new(&self.kustomize)
                .arg("build")
                .arg(directory.to_string_lossy()))
        }
    }
    pub fn kustomize_apply(&self, directory: &Path, dry_run: bool) -> anyhow::Result<CommandSpec> {
        let mut args = vec![
            "apply".into(),
            "-k".into(),
            directory.to_string_lossy().into_owned(),
        ];
        if dry_run {
            args.extend(["--dry-run=server".into(), "-o=yaml".into()]);
        }
        self.kubectl(args)
    }
    pub fn manifest_apply(&self, path: &Path, dry_run: bool) -> anyhow::Result<CommandSpec> {
        let mut args = vec![
            "apply".into(),
            "-f".into(),
            path.to_string_lossy().into_owned(),
        ];
        if dry_run {
            args.extend(["--dry-run=server".into(), "-o=yaml".into()]);
        }
        self.kubectl(args)
    }
    pub fn helm_release(
        &self,
        release: &str,
        chart: &str,
        values: &str,
        operation: &str,
        revision: u32,
    ) -> anyhow::Result<CommandSpec> {
        identifier(release)?;
        let mut args = match operation {
            "render" => vec!["template".into(), release.into(), chart.into()],
            "plan" | "install" => vec![
                "upgrade".into(),
                "--install".into(),
                release.into(),
                chart.into(),
                "--wait".into(),
                "--timeout=120s".into(),
            ],
            "uninstall" => vec!["uninstall".into(), release.into(), "--wait".into()],
            "history" => vec!["history".into(), release.into(), "-o=json".into()],
            "rollback" => {
                anyhow::ensure!(revision > 0, "Select a positive Helm revision");
                vec![
                    "rollback".into(),
                    release.into(),
                    revision.to_string(),
                    "--wait".into(),
                ]
            }
            _ => anyhow::bail!("Unsupported Helm action"),
        };
        if matches!(operation, "render" | "plan" | "install") {
            anyhow::ensure!(
                !chart.is_empty() && !chart.starts_with('-'),
                "Enter a chart path, URL or repository/chart"
            );
            if !values.is_empty() {
                args.extend(["--values".into(), values.into()]);
            }
        }
        if operation == "plan" {
            args.push("--dry-run=server".into());
        }
        self.helm(args)
    }
}
fn identifier(value: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        !value.is_empty()
            && value.len() <= 253
            && value
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
            && !value.starts_with('-'),
        "Select a valid resource name"
    );
    Ok(())
}
fn resource_kind(value: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        matches!(
            value,
            "pods"
                | "services"
                | "deployments"
                | "statefulsets"
                | "daemonsets"
                | "jobs"
                | "cronjobs"
                | "configmaps"
                | "secrets"
                | "ingresses"
                | "nodes"
                | "namespaces"
                | "persistentvolumeclaims"
                | "events"
        ),
        "Select a supported resource kind"
    );
    Ok(())
}
pub const KINDS: &[&str] = &[
    "pods",
    "services",
    "deployments",
    "statefulsets",
    "daemonsets",
    "jobs",
    "cronjobs",
    "configmaps",
    "secrets",
    "ingresses",
    "nodes",
    "namespaces",
    "persistentvolumeclaims",
    "events",
];
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Resource {
    pub kind: String,
    pub name: String,
    pub namespace: String,
    pub phase: String,
    pub containers: Vec<String>,
}
pub fn resources(json: &str) -> Result<Vec<Resource>, String> {
    let value: serde_json::Value = serde_json::from_str(json).map_err(|e| e.to_string())?;
    let list = value
        .get("items")
        .and_then(|v| v.as_array())
        .ok_or("Expected a Kubernetes List response")?;
    Ok(list
        .iter()
        .take(20000)
        .filter_map(|item| {
            let name = item.pointer("/metadata/name")?.as_str()?.to_owned();
            Some(Resource {
                kind: item
                    .get("kind")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .into(),
                name,
                namespace: item
                    .pointer("/metadata/namespace")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .into(),
                phase: item
                    .pointer("/status/phase")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .into(),
                containers: item
                    .pointer("/spec/containers")
                    .and_then(|v| v.as_array())
                    .map(|items| {
                        items
                            .iter()
                            .filter_map(|v| v.get("name")?.as_str().map(str::to_owned))
                            .collect()
                    })
                    .unwrap_or_default(),
            })
        })
        .collect())
}
/// Resource YAML is displayed/exported without Kubernetes Secret payloads.
pub fn redact(value: &str) -> String {
    let Ok(mut data) = serde_json::from_str::<serde_json::Value>(value) else {
        return value.into();
    };
    fn visit(value: &mut serde_json::Value) {
        match value {
            serde_json::Value::Object(map) => {
                if map.get("kind").and_then(|v| v.as_str()) == Some("Secret") {
                    map.remove("data");
                    map.remove("stringData");
                    if let Some(metadata) = map
                        .get_mut("metadata")
                        .and_then(|value| value.as_object_mut())
                    {
                        metadata.remove("annotations");
                    }
                }
                for value in map.values_mut() {
                    visit(value)
                }
            }
            serde_json::Value::Array(list) => {
                for value in list {
                    visit(value)
                }
            }
            _ => {}
        }
    }
    visit(&mut data);
    serde_json::to_string_pretty(&data).unwrap_or_default()
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn captured_target_and_loopback_are_preserved() {
        let target = Target {
            context: "production".into(),
            namespace: "app".into(),
            kubeconfig: "/home/me/config with spaces".into(),
            ..Default::default()
        };
        let command = target.port_forward("services", "api", 8080, 80).unwrap();
        assert!(command.args.contains(&"--address=127.0.0.1".into()));
        assert!(command.args.contains(&"--context=production".into()));
        assert!(command.args.contains(&"8080:80".into()));
        assert!(target.port_forward("pods", "--bad", 1, 2).is_err());
    }
    #[test]
    fn plans_are_distinct_from_mutations() {
        let target = Target {
            context: "fixture-cluster".into(),
            ..Default::default()
        };
        assert!(target
            .kustomize_apply(Path::new("overlays/dev"), true)
            .unwrap()
            .args
            .contains(&"--dry-run=server".into()));
        assert!(!target
            .kustomize_apply(Path::new("overlays/dev"), false)
            .unwrap()
            .args
            .contains(&"--dry-run=server".into()));
        assert!(target
            .helm_release("api", "charts/api", "values.yaml", "plan", 0)
            .unwrap()
            .args
            .contains(&"--dry-run=server".into()));
    }
    #[test]
    fn resource_identity_and_secret_redaction() {
        let value = r#"{"items":[{"kind":"Pod","metadata":{"name":"api-a","namespace":"app"},"status":{"phase":"Running"},"spec":{"containers":[{"name":"web"}]}}]}"#;
        let rows = resources(value).unwrap();
        assert_eq!(rows[0].containers, vec!["web"]);
        assert_eq!(rows[0].namespace, "app");
        let redacted = redact(r#"{"kind":"Secret","data":{"password":"hidden"}}"#);
        assert!(!redacted.contains("hidden"));
    }
}

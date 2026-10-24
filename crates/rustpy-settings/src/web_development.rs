//! Installed standard toolchains; credentials and installations are not invented.
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Server {
    pub enabled: bool,
    pub executable: String,
    pub arguments: Vec<String>,
    pub initialization: serde_json::Value,
}
impl Default for Server {
    fn default() -> Self {
        Self {
            enabled: true,
            executable: String::new(),
            arguments: vec!["--stdio".into()],
            initialization: serde_json::json!({}),
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct WebDevelopment {
    pub enabled: bool,
    pub automatic: bool,
    pub servers: BTreeMap<String, Server>,
    pub node: String,
    pub npm: String,
    pub javascript_adapter: String,
    pub adapter_arguments: Vec<String>,
    pub configuration: serde_json::Value,
}
impl Default for WebDevelopment {
    fn default() -> Self {
        let mut servers = BTreeMap::new();
        for (name, executable) in [
            ("typescript", "typescript-language-server"),
            ("html", "vscode-html-language-server"),
            ("css", "vscode-css-language-server"),
            ("json", "vscode-json-language-server"),
            ("vue", "vue-language-server"),
            ("angular", "ngserver"),
            ("svelte", "svelteserver"),
            ("eslint", "vscode-eslint-language-server"),
        ] {
            let mut server = Server {
                executable: executable.into(),
                ..Default::default()
            };
            if name == "vue" {
                server.initialization = serde_json::json!({"vue":{"hybridMode":false}});
            }
            if name == "eslint" {
                server.enabled = false;
            }
            servers.insert(name.into(), server);
        }
        Self {
            enabled: true,
            automatic: false,
            servers,
            node: "node".into(),
            npm: "npm".into(),
            javascript_adapter: String::new(),
            adapter_arguments: vec!["$Port$".into()],
            configuration: serde_json::json!({"typescript":{"format":{"enable":true}},"javascript":{"format":{"enable":true}},"eslint":{"validate":"on","run":"onType","workingDirectory":{"mode":"auto"}}}),
        }
    }
}

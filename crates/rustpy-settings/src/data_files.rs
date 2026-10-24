use serde::{Deserialize, Serialize};
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SchemaMapping {
    pub pattern: String,
    pub schema: String,
    pub enabled: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DataFileSettings {
    pub schemas: Vec<SchemaMapping>,
    pub shellcheck: String,
    pub shfmt: String,
    pub xmllint: String,
    pub toml_formatter: String,
    pub csv_header: bool,
    pub archive_limit_mib: usize,
}
impl Default for DataFileSettings {
    fn default() -> Self {
        Self {
            schemas: Vec::new(),
            shellcheck: "shellcheck".into(),
            shfmt: "shfmt".into(),
            xmllint: "xmllint".into(),
            toml_formatter: "taplo".into(),
            csv_header: true,
            archive_limit_mib: 64,
        }
    }
}
pub fn matches(pattern: &str, path: &std::path::Path) -> bool {
    let text = path.to_string_lossy().replace('\\', "/");
    fn glob(pattern: &[u8], text: &[u8]) -> bool {
        let (mut p, mut t, mut star, mut mark) = (0, 0, None, 0);
        while t < text.len() {
            if p < pattern.len() && (pattern[p] == b'?' || pattern[p] == text[t]) {
                p += 1;
                t += 1;
            } else if p < pattern.len() && pattern[p] == b'*' {
                star = Some(p);
                p += 1;
                mark = t;
            } else if let Some(back) = star {
                p = back + 1;
                mark += 1;
                t = mark;
            } else {
                return false;
            }
        }
        while p < pattern.len() && pattern[p] == b'*' {
            p += 1;
        }
        p == pattern.len()
    }
    glob(pattern.as_bytes(), text.as_bytes())
        || path
            .file_name()
            .is_some_and(|name| glob(pattern.as_bytes(), name.to_string_lossy().as_bytes()))
}

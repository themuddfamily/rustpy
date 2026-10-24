//! Bounded OpenSSH Host/Include configuration resolution. No ssh process or connection.
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Resolved {
    pub alias: String,
    pub hostname: String,
    pub user: String,
    pub port: u16,
    pub identity: Option<PathBuf>,
    pub proxy_jump: String,
}
fn expand(path: &str) -> PathBuf {
    if path == "~" || path.starts_with("~/") {
        let home = std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .unwrap_or_default();
        PathBuf::from(home).join(path.trim_start_matches('~').trim_start_matches('/'))
    } else {
        path.into()
    }
}
pub fn resolve(file: &Path, alias: &str) -> anyhow::Result<Resolved> {
    let mut values = BTreeMap::new();
    read(file, alias, &mut values, &mut BTreeSet::new(), 0)?;
    Ok(Resolved {
        alias: alias.into(),
        hostname: values.remove("hostname").unwrap_or_else(|| alias.into()),
        user: values.remove("user").unwrap_or_default(),
        port: values
            .remove("port")
            .map(|value| value.parse())
            .transpose()?
            .unwrap_or(22),
        identity: values
            .remove("identityfile")
            .map(|path| expand(&path.replace("%h", alias))),
        proxy_jump: values.remove("proxyjump").unwrap_or_default(),
    })
}
fn read(
    file: &Path,
    alias: &str,
    values: &mut BTreeMap<String, String>,
    seen: &mut BTreeSet<PathBuf>,
    depth: usize,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        depth < 16 && seen.len() < 128,
        "SSH configuration includes exceed their limit"
    );
    let path = expand(&file.to_string_lossy());
    if !seen.insert(path.clone()) {
        return Ok(());
    }
    let bytes = std::fs::read(&path)?;
    anyhow::ensure!(bytes.len() <= 1024 * 1024, "SSH configuration is too large");
    let text = String::from_utf8(bytes)?;
    let mut active = true;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once(|c: char| c.is_whitespace() || c == '=') else {
            continue;
        };
        let key = key.to_ascii_lowercase();
        let value = value
            .trim_start_matches(['=', ' ', '\t'])
            .trim()
            .trim_matches('"');
        match key.as_str() {
            "host" => {
                active = false;
                for pattern in value.split_whitespace() {
                    if let Some(negative) = pattern.strip_prefix('!') {
                        if crate::glob_match(negative, alias) {
                            active = false;
                            break;
                        }
                    } else if crate::glob_match(pattern, alias) {
                        active = true;
                    }
                }
            }
            "match" => active = false,
            "include" if active => {
                for include in value.split_whitespace() {
                    let include = expand(include);
                    let include = if include.is_absolute() {
                        include
                    } else {
                        path.parent().unwrap_or(Path::new(".")).join(include)
                    };
                    if include.to_string_lossy().contains(['*', '?']) {
                        if let Some(parent) = include.parent() {
                            if let Ok(entries) = std::fs::read_dir(parent) {
                                let mut files = entries
                                    .flatten()
                                    .map(|entry| entry.path())
                                    .filter(|entry| {
                                        crate::glob_match(
                                            &include
                                                .file_name()
                                                .unwrap_or_default()
                                                .to_string_lossy(),
                                            &entry
                                                .file_name()
                                                .unwrap_or_default()
                                                .to_string_lossy(),
                                        )
                                    })
                                    .take(128)
                                    .collect::<Vec<_>>();
                                files.sort();
                                for file in files {
                                    read(&file, alias, values, seen, depth + 1)?;
                                }
                            }
                        }
                    } else {
                        read(&include, alias, values, seen, depth + 1)?;
                    }
                }
            }
            _ if active => {
                values.entry(key).or_insert_with(|| value.into());
            }
            _ => {}
        }
    }
    Ok(())
}
#[cfg(test)]
mod fixtures {
    use super::*;
    #[test]
    fn agent_config_expansion_does_not_execute_percent_commands() {
        let path = expand("/keys/%h");
        assert_eq!(path, PathBuf::from("/keys/%h"));
    }
}
#[cfg(test)]
mod config_fixtures {
    use super::*;
    #[test]
    fn configured_include_keeps_first_values_and_resolves_jump_host() {
        let directory = crate::test_util::TempDir::new("ssh-config-source");
        let file = directory.path().join("config");
        std::fs::write(&file,"Host prod\n HostName server.example\n User first\n Include extra\nHost *\n Port 2222\n").unwrap();
        std::fs::write(
            directory.path().join("extra"),
            "Host prod\n User second\n ProxyJump bastion\n IdentityFile /keys/deploy\n",
        )
        .unwrap();
        let resolved = resolve(&file, "prod").unwrap();
        assert_eq!(resolved.hostname, "server.example");
        assert_eq!(resolved.user, "first");
        assert_eq!(resolved.port, 2222);
        assert_eq!(resolved.proxy_jump, "bastion");
        assert_eq!(resolved.identity, Some("/keys/deploy".into()));
    }
}

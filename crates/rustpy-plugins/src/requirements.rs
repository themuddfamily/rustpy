//! Project plugin requirements are advisory data. They never install, enable,
//! grant permissions or execute plugins merely because a project was opened.
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, io::Read, path::Path};
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Requirements {
    pub required: Vec<Required>,
    pub suggested: BTreeMap<String, Vec<String>>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Required {
    pub id: String,
    #[serde(default)]
    pub minimum_version: Option<String>,
}
impl Requirements {
    pub fn load(root: &Path) -> Result<Self, String> {
        let path = root.join(".rustpy/plugins.json");
        if !path.exists() {
            return Ok(Self::default());
        }
        let path = crate::paths::existing_file_inside(root, ".rustpy/plugins.json")?;
        let mut data = Vec::new();
        std::fs::File::open(&path)
            .map_err(|e| e.to_string())?
            .take(65537)
            .read_to_end(&mut data)
            .map_err(|e| e.to_string())?;
        if data.len() > 65536 {
            return Err("Project plugin requirements exceed 64 KiB".into());
        }
        let requirements: Self = serde_json::from_slice(&data).map_err(|e| e.to_string())?;
        if requirements.required.len() > 128 || requirements.suggested.len() > 128 {
            return Err("Too many project plugin requirements".into());
        }
        for required in &requirements.required {
            crate::validate_plugin_id(&required.id)?;
            if let Some(version) = &required.minimum_version {
                crate::Version::parse(version)?;
            }
        }
        for (extension, plugins) in &requirements.suggested {
            crate::manifest::normalize_extension(extension)?;
            if plugins.len() > 32 {
                return Err("Too many suggested plugins for one file type".into());
            }
            for plugin in plugins {
                crate::validate_plugin_id(plugin)?;
            }
        }
        Ok(requirements)
    }
    pub fn missing(&self, registry: &crate::PluginRegistry) -> Vec<String> {
        self.required
            .iter()
            .filter(|required| {
                !registry.get(&required.id).is_some_and(|plugin| {
                    plugin.is_active()
                        && required.minimum_version.as_ref().is_none_or(|minimum| {
                            crate::compare_versions(&plugin.manifest.version, minimum)
                                .is_some_and(|order| order != std::cmp::Ordering::Less)
                        })
                })
            })
            .map(|required| {
                format!(
                    "{}{}",
                    required.id,
                    required
                        .minimum_version
                        .as_ref()
                        .map(|version| format!(" >= {version}"))
                        .unwrap_or_default()
                )
            })
            .collect()
    }
}
#[cfg(test)]
mod fixtures {
    use super::*;
    #[test]
    fn project_file_cannot_smuggle_permission_or_install_instructions() {
        assert!(serde_json::from_str::<Requirements>(
            r#"{"required":[],"execute":"python setup.py"}"#
        )
        .is_err());
        let directory = tempfile::tempdir().unwrap();
        std::fs::create_dir(directory.path().join(".rustpy")).unwrap();
        std::fs::write(directory.path().join(".rustpy/plugins.json"),r#"{"required":[{"id":"com.example.plugin","minimum_version":"1.0.0"}],"suggested":{"abc":["com.example.plugin"]}}"#).unwrap();
        let requirements = Requirements::load(directory.path()).unwrap();
        assert_eq!(requirements.required.len(), 1);
        assert_eq!(requirements.suggested["abc"].len(), 1);
    }
}

//! Persisted run-dialog preferences and ordered, bounded before-launch plans.
use crate::RunConfig;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, HashSet},
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Workflow {
    pub folder: String,
    pub allow_parallel: bool,
    pub show_settings: bool,
    pub activate_window: bool,
    pub focus_window: bool,
    pub inherit_environment: bool,
    pub pinned: bool,
    pub last_used: u64,
    pub before_launch: Vec<BeforeLaunch>,
}
impl Default for Workflow {
    fn default() -> Self {
        Self {
            folder: String::new(),
            allow_parallel: false,
            show_settings: false,
            activate_window: true,
            focus_window: false,
            inherit_environment: true,
            pinned: false,
            last_used: 0,
            before_launch: Vec::new(),
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct BeforeLaunch {
    pub enabled: bool,
    pub task: Task,
}
impl Default for BeforeLaunch {
    fn default() -> Self {
        Self {
            enabled: true,
            task: Task::Configuration {
                name: String::new(),
            },
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Task {
    Configuration {
        name: String,
    },
    ExternalTool {
        name: String,
    },
    FileWatcher {
        name: String,
    },
    Upload {
        server: String,
        path: PathBuf,
    },
    Npm {
        directory: PathBuf,
        script: String,
    },
    Grunt {
        directory: PathBuf,
        task: String,
    },
    Command {
        program: PathBuf,
        arguments: String,
        directory: PathBuf,
    },
}
#[derive(Clone, Debug)]
pub enum Planned {
    Configuration(Box<RunConfig>),
    Task(Task),
}
/// The same configuration may be requested twice. Only ancestors constitute
/// a cycle; enabled tasks retain their configured order and actual arguments.
pub fn before_plan(
    configuration: &RunConfig,
    configurations: &[RunConfig],
) -> Result<Vec<Planned>, String> {
    fn visit(
        configuration: &RunConfig,
        all: &[RunConfig],
        stack: &mut HashSet<String>,
        output: &mut Vec<Planned>,
        depth: usize,
        include_target: bool,
    ) -> Result<(), String> {
        if depth > 16 || output.len() > 128 {
            return Err("Before-launch plan exceeds 16 nested configurations or 128 tasks".into());
        }
        if !stack.insert(configuration.name.clone()) {
            return Err(format!(
                "Before-launch configuration cycle includes '{}'",
                configuration.name
            ));
        }
        for entry in configuration
            .workflow
            .before_launch
            .iter()
            .filter(|entry| entry.enabled)
        {
            if output.len() >= 128 {
                return Err("Before-launch plan exceeds 128 tasks".into());
            }
            match &entry.task {
                Task::Configuration { name } => {
                    let next = all
                        .iter()
                        .find(|candidate| candidate.name == *name)
                        .ok_or_else(|| {
                            format!("Before-launch configuration '{name}' does not exist")
                        })?;
                    visit(next, all, stack, output, depth + 1, true)?;
                }
                task => output.push(Planned::Task(task.clone())),
            }
        }
        if include_target {
            if let Some(spec) = configuration
                .type_options
                .as_ref()
                .filter(|spec| spec.kind == crate::configuration_types::ConfigurationType::Compound)
            {
                for name in &spec.configurations {
                    let member = all
                        .iter()
                        .find(|cfg| &cfg.name == name)
                        .ok_or_else(|| format!("Compound member '{name}' does not exist"))?;
                    visit(member, all, stack, output, depth + 1, true)?;
                }
            } else {
                if output.len() >= 128 {
                    return Err("Before-launch plan exceeds 128 tasks".into());
                }
                output.push(Planned::Configuration(Box::new(configuration.clone())));
            }
        }
        stack.remove(&configuration.name);
        Ok(())
    }
    let mut output = Vec::new();
    visit(
        configuration,
        configurations,
        &mut HashSet::new(),
        &mut output,
        0,
        false,
    )?;
    Ok(output)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Templates {
    pub defaults: BTreeMap<String, RunConfig>,
    pub temporary_limit: usize,
}
impl Default for Templates {
    fn default() -> Self {
        Self {
            defaults: BTreeMap::new(),
            temporary_limit: 5,
        }
    }
}
impl Templates {
    fn key(configuration: &RunConfig) -> &'static str {
        use crate::RunConfigKind;
        configuration
            .type_options
            .as_ref()
            .map(|spec| spec.label())
            .unwrap_or_else(|| match &configuration.kind {
                RunConfigKind::Python { .. } => "Python script",
                RunConfigKind::PythonModule { module } if module == "doctest" => "Doctests",
                RunConfigKind::PythonModule { .. } => "Python module",
                RunConfigKind::Pytest { .. } => "pytest",
                RunConfigKind::Unittest { .. } => "Unittests",
                RunConfigKind::ShellScript { .. } => "Shell Script",
                RunConfigKind::Program { .. } => "Program",
            })
    }
    pub fn path(root: &Path) -> PathBuf {
        root.join(".rustpy/runConfigurations/templates.json")
    }
    pub fn load(root: &Path) -> Self {
        use std::io::Read;
        std::fs::File::open(Self::path(root))
            .ok()
            .and_then(|file| {
                let mut bytes = Vec::new();
                file.take(4 * 1024 * 1024 + 1)
                    .read_to_end(&mut bytes)
                    .ok()?;
                (bytes.len() <= 4 * 1024 * 1024).then_some(bytes)
            })
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }
    pub fn save(&self, root: &Path) -> Result<(), String> {
        let path = Self::path(root);
        let parent = path.parent().ok_or("Missing template directory")?;
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        let data = serde_json::to_vec_pretty(self).map_err(|error| error.to_string())?;
        let stage = path.with_extension("json.staging");
        std::fs::write(&stage, data).map_err(|error| error.to_string())?;
        std::fs::rename(stage, path).map_err(|error| error.to_string())
    }
    pub fn apply(&self, configuration: &RunConfig) -> RunConfig {
        let Some(default) = self
            .defaults
            .get(Self::key(configuration))
            .or_else(|| self.defaults.get(configuration.kind_label()))
        else {
            return configuration.clone();
        };
        let mut result = default.clone();
        result.name = configuration.name.clone();
        result.kind = configuration.kind.clone();
        result.type_options = match (&default.type_options, &configuration.type_options) {
            (Some(template), Some(context)) if template.kind == context.kind => {
                Some(template.clone())
            }
            _ => configuration.type_options.clone(),
        };
        result.temporary = configuration.temporary;
        result.shared = configuration.shared;
        result
    }
    pub fn remember(&mut self, configuration: &RunConfig) {
        self.defaults
            .insert(Self::key(configuration).into(), configuration.clone());
    }
    pub fn trim_temporary(&self, configurations: &mut Vec<RunConfig>, selected: Option<&str>) {
        let limit = self.temporary_limit.clamp(1, 100);
        while configurations
            .iter()
            .filter(|configuration| configuration.temporary && !configuration.workflow.pinned)
            .count()
            > limit
        {
            let Some(index) = configurations.iter().position(|configuration| {
                configuration.temporary
                    && !configuration.workflow.pinned
                    && Some(configuration.name.as_str()) != selected
            }) else {
                break;
            };
            configurations.remove(index);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn compound_prerequisites_cannot_hide_cycles_or_exceed_task_bound() {
        use crate::configuration_types::{ConfigurationSpec, ConfigurationType};
        let mut parent = RunConfig {
            name: "main".into(),
            ..Default::default()
        };
        let mut compound = RunConfig {
            name: "all".into(),
            type_options: Some(ConfigurationSpec::new(ConfigurationType::Compound)),
            ..Default::default()
        };
        parent.workflow.before_launch.push(BeforeLaunch {
            enabled: true,
            task: Task::Configuration {
                name: compound.name.clone(),
            },
        });
        compound
            .type_options
            .as_mut()
            .unwrap()
            .configurations
            .push(parent.name.clone());
        assert!(before_plan(&parent, &[parent.clone(), compound])
            .unwrap_err()
            .contains("cycle"));
        parent.workflow.before_launch = vec![
            BeforeLaunch {
                enabled: true,
                task: Task::Command {
                    program: "echo".into(),
                    arguments: String::new(),
                    directory: PathBuf::new()
                }
            };
            129
        ];
        assert!(before_plan(&parent, &[]).unwrap_err().contains("128"));
    }
    #[test]
    fn before_graph_preserves_order_repeats_disabled_entries_and_rejects_cycles() {
        let child = RunConfig {
            name: "build".into(),
            ..Default::default()
        };
        let mut parent = RunConfig {
            name: "run".into(),
            ..Default::default()
        };
        let entry = BeforeLaunch {
            enabled: true,
            task: Task::Configuration {
                name: child.name.clone(),
            },
        };
        parent.workflow.before_launch = vec![
            entry.clone(),
            BeforeLaunch {
                enabled: false,
                task: Task::Configuration {
                    name: "missing".into(),
                },
            },
            entry,
        ];
        assert_eq!(
            before_plan(&parent, std::slice::from_ref(&child))
                .unwrap()
                .len(),
            2
        );
        let mut child = child;
        child.workflow.before_launch.push(BeforeLaunch {
            enabled: true,
            task: Task::Configuration {
                name: parent.name.clone(),
            },
        });
        assert!(before_plan(&parent, &[child, parent.clone()])
            .unwrap_err()
            .contains("cycle"));
    }
    #[test]
    fn defaults_copy_execution_preferences_while_preserving_new_target_and_pins() {
        let mut defaults = Templates::default();
        let mut template = RunConfig {
            name: "template".into(),
            parameters: "--flag".into(),
            ..Default::default()
        };
        template.workflow.folder = "Web".into();
        defaults.remember(&template);
        let context = RunConfig::for_script(Path::new("/project/main.py"));
        let applied = defaults.apply(&context);
        assert_eq!(applied.kind, context.kind);
        assert_eq!(applied.name, context.name);
        assert_eq!(applied.parameters, "--flag");
        let mut configurations = (0..8)
            .map(|index| RunConfig {
                name: index.to_string(),
                temporary: true,
                ..Default::default()
            })
            .collect::<Vec<_>>();
        configurations[0].workflow.pinned = true;
        defaults.temporary_limit = 2;
        defaults.trim_temporary(&mut configurations, Some("7"));
        assert_eq!(configurations.len(), 3);
        assert!(configurations
            .iter()
            .any(|configuration| configuration.name == "0"));
        assert!(configurations
            .iter()
            .any(|configuration| configuration.name == "7"));
    }
}

//! Sphinx/Docutils launches and captured extension registries. No shell commands.
use crate::{RunConfig, RunConfigKind};
use rustpy_settings::sphinx::SphinxSettings;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};
fn resolve(root: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.into()
    } else {
        root.join(path)
    }
}
fn quote(value: &str) -> String {
    // RunConfig's parser concatenates adjacent quoted segments; backslashes
    // are literal (important for Windows paths), not shell escape characters.
    format!("\"{}\"", value.replace('"', "\"'\"'\""))
}
pub fn build_configuration(root: &Path, prefs: &SphinxSettings) -> Result<RunConfig, String> {
    let cwd = prefs
        .working_directory(root)
        .ok_or("Choose a Sphinx working directory in Python Integrated Tools")?;
    if !cwd.is_dir() {
        return Err("Sphinx working directory does not exist".into());
    }
    let source = resolve(&cwd, &prefs.source_directory);
    let conf = resolve(&cwd, &prefs.configuration_directory);
    if !source.is_dir() || !conf.join("conf.py").is_file() {
        return Err("Sphinx source directory or configuration conf.py is missing".into());
    }
    if prefs.builder.is_empty()
        || !prefs
            .builder
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
    {
        return Err("Invalid Sphinx builder name".into());
    }
    let output = resolve(&cwd, &prefs.output_directory);
    if source == output {
        return Err("Sphinx output directory must differ from its source directory".into());
    }
    let mut args = vec![
        "-b".into(),
        prefs.builder.clone(),
        "-c".into(),
        conf.to_string_lossy().into_owned(),
    ];
    if prefs.warnings_as_errors {
        args.push("-W".into());
    }
    if prefs.fresh_environment {
        args.push("-E".into());
    }
    args.extend(RunConfig::split_args(&prefs.additional_arguments));
    args.extend([
        source.to_string_lossy().into_owned(),
        output.to_string_lossy().into_owned(),
    ]);
    Ok(RunConfig {
        name: format!("Sphinx ({})", prefs.builder),
        kind: RunConfigKind::PythonModule {
            module: "sphinx".into(),
        },
        parameters: args.iter().map(|s| quote(s)).collect::<Vec<_>>().join(" "),
        working_dir: Some(cwd),
        temporary: false,
        ..Default::default()
    })
}
pub fn docutils_configuration(
    source: &Path,
    output: &Path,
    interpreter: &Path,
) -> Result<RunConfig, String> {
    if !source.is_file() || source == output {
        return Err(
            "Choose a saved reStructuredText source and a different HTML output file".into(),
        );
    }
    Ok(RunConfig {
        name: format!(
            "Docutils: {}",
            source.file_name().unwrap_or_default().to_string_lossy()
        ),
        kind: RunConfigKind::Program {
            program: interpreter.into(),
            args: vec![
                "-c".into(),
                "from docutils.core import publish_cmdline; publish_cmdline(writer_name='html5')"
                    .into(),
                source.to_string_lossy().into_owned(),
                output.to_string_lossy().into_owned(),
            ],
        },
        working_dir: source.parent().map(Path::to_path_buf),
        temporary: false,
        ..Default::default()
    })
}
/// HTML produced by the configured builder for the selected source document.
pub fn preview_path(root: &Path, prefs: &SphinxSettings, source: &Path) -> Option<PathBuf> {
    let cwd = prefs.working_directory(root)?;
    let source_root = resolve(&cwd, &prefs.source_directory);
    let relative = source.strip_prefix(&source_root).ok()?;
    let output = resolve(&cwd, &prefs.output_directory);
    match prefs.builder.as_str() {
        "html" => Some(output.join(relative).with_extension("html")),
        "dirhtml" => Some(
            if relative.file_stem().is_some_and(|name| name == "index") {
                output.join(relative).with_extension("html")
            } else {
                output.join(relative).with_extension("").join("index.html")
            },
        ),
        "singlehtml" => Some(output.join("index.html")),
        _ => None,
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Registry {
    pub directives: Vec<String>,
    pub roles: Vec<String>,
    pub references: Vec<String>,
}
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct RegistryKey {
    source: PathBuf,
    configuration: PathBuf,
    python: PathBuf,
    revision: u64,
}
type Cache = HashMap<RegistryKey, (Instant, Registry)>;
static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();
static ACTIVE: Mutex<bool> = Mutex::new(false);
/// Returns captured custom directives/roles; starts at most one trusted worker.
pub fn registry(
    root: &Path,
    python: &Path,
    revision: u64,
    trusted: bool,
    preferences: &SphinxSettings,
) -> Registry {
    if !trusted {
        return Registry::default();
    }
    let key = RegistryKey {
        source: resolve(root, &preferences.source_directory),
        configuration: resolve(root, &preferences.configuration_directory),
        python: python.to_path_buf(),
        revision,
    };
    let cache = CACHE.get_or_init(Default::default);
    if let Ok(cache) = cache.try_lock() {
        if let Some((at, value)) = cache.get(&key) {
            if at.elapsed() < Duration::from_secs(60) {
                return value.clone();
            }
        }
    }
    let Ok(mut active) = ACTIVE.try_lock() else {
        return Registry::default();
    };
    if *active {
        return Registry::default();
    }
    *active = true;
    drop(active);
    rustpy_core::TaskManager::global()
        .task("Loading Sphinx directive and role registry")
        .spawn(move |progress| {
            struct Release;
            impl Drop for Release {
                fn drop(&mut self) {
                    if let Ok(mut active) = ACTIVE.lock() {
                        *active = false;
                    }
                }
            }
            let _release = Release;
            // Rust owns the scratch directory so cancellation/forced child
            // termination still removes generated documentation afterwards.
            static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let scratch = key.source.join(".rustpy/sphinx-registry").join(format!(
                "{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            ));
            if std::fs::create_dir_all(&scratch).is_err() {
                return Ok(());
            }
            struct Scratch(PathBuf);
            impl Drop for Scratch {
                fn drop(&mut self) {
                    let _ = std::fs::remove_dir_all(&self.0);
                }
            }
            let _scratch = Scratch(scratch.clone());
            let mut command = crate::interpreter::interpreter_command(&key.python);
            command
                .args(["-c", REGISTRY])
                .arg(&key.source)
                .arg(&scratch)
                .arg(&key.configuration)
                .current_dir(&key.source)
                .env("PYTHONDONTWRITEBYTECODE", "1");
            let value = crate::native_skeleton::supervise_tool(
                &mut command,
                progress.token(),
                Duration::from_secs(4),
                262144,
            )
            .ok()
            .and_then(|(out, _)| {
                String::from_utf8_lossy(&out)
                    .lines()
                    .rev()
                    .find_map(|line| {
                        line.strip_prefix("RUSTPY_RST:")
                            .and_then(|s| serde_json::from_str(s).ok())
                    })
            })
            .unwrap_or_default();
            if let Ok(mut cache) = cache.lock() {
                if cache.len() >= 8 {
                    cache.clear();
                }
                cache.insert(key, (Instant::now(), value));
            }
            Ok(())
        });
    Registry::default()
}
const REGISTRY: &str = r#"
import sys,json,pathlib,itertools
result={'directives':[],'roles':[],'references':[]};root=pathlib.Path(sys.argv[1]);conf=pathlib.Path(sys.argv[3])
try:
 from docutils.parsers.rst import directives,roles
 # Importing language tables gives standard names without executing a document.
 from docutils.parsers.rst.languages import en
 result['directives']=list(en.directives);result['roles']=list(en.roles)
 if (conf/'conf.py').is_file():
  from sphinx.application import Sphinx
  output=sys.argv[2]
  app=Sphinx(str(root),str(conf),output,output+'/doctrees','dummy',status=None,warning=None,freshenv=True)
  app.build(force_all=True)
  for name,domain in itertools.islice(app.registry.domains.items(),64):
   result['directives'] += [name+':'+key for key in itertools.islice(domain.directives,max(0,2048-len(result['directives'])))]
   result['roles'] += [name+':'+key for key in itertools.islice(domain.roles,max(0,2048-len(result['roles'])))]
  for domain in itertools.islice(app.env.domains.values(),64):
   result['references'] += [str(obj[0]) for obj in itertools.islice(domain.get_objects(),max(0,2048-len(result['references'])))]
 result['directives']+=list(directives._directives)
 result['roles']+=list(roles._roles)
except Exception:pass
for key in result:result[key]=sorted(set(value for value in result[key] if len(value)<=256))[:2048]
print('RUSTPY_RST:'+json.dumps(result))
"#;
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn untrusted_registry_is_non_executing() {
        let registry = registry(
            Path::new("/no-such-project"),
            Path::new("/no-python"),
            0,
            false,
            &SphinxSettings::default(),
        );
        assert!(registry.directives.is_empty() && registry.roles.is_empty());
    }
    #[test]
    fn quoting_matches_the_run_configuration_parser() {
        for value in [r"C:\docs path\source", "a\"b'c", "white space", ""] {
            assert_eq!(
                RunConfig::split_args(&quote(value)),
                vec![value.to_string()]
            );
        }
    }
    #[test]
    fn sphinx_launch_preserves_argument_boundaries_and_flags() {
        let root = tempfile::tempdir().unwrap();
        let docs = root.path().join("docs space");
        std::fs::create_dir(&docs).unwrap();
        std::fs::write(docs.join("conf.py"), "").unwrap();
        let prefs = SphinxSettings {
            working_directory: "docs space".into(),
            warnings_as_errors: true,
            fresh_environment: true,
            ..Default::default()
        };
        let config = build_configuration(root.path(), &prefs).unwrap();
        let args = RunConfig::split_args(&config.parameters);
        assert_eq!(args[0..2], ["-b", "html"]);
        assert!(args.contains(&"-W".into()));
        assert!(args.contains(&"-E".into()));
        assert_eq!(
            args.last().unwrap(),
            &docs.join("_build/html").to_string_lossy()
        );
        assert!(!config.temporary);
        assert_eq!(
            preview_path(root.path(), &prefs, &docs.join("guide.rst")),
            Some(docs.join("_build/html/guide.html"))
        );
        let dirhtml = SphinxSettings {
            builder: "dirhtml".into(),
            ..prefs.clone()
        };
        assert_eq!(
            preview_path(root.path(), &dirhtml, &docs.join("guide.rst")),
            Some(docs.join("_build/html/guide/index.html"))
        );
        assert_eq!(
            preview_path(root.path(), &dirhtml, &docs.join("index.rst")),
            Some(docs.join("_build/html/index.html"))
        );
        let invalid = SphinxSettings {
            builder: "html; command".into(),
            ..prefs
        };
        assert!(build_configuration(root.path(), &invalid).is_err());
    }
    #[test]
    fn docutils_is_direct_argv_and_rejects_in_place_output() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("with space.rst");
        std::fs::write(&source, "Title\n=====\n").unwrap();
        assert!(docutils_configuration(&source, &source, Path::new("python")).is_err());
        let cfg = docutils_configuration(
            &source,
            &dir.path().join("result.html"),
            Path::new("python"),
        )
        .unwrap();
        let RunConfigKind::Program { args, .. } = cfg.kind else {
            panic!()
        };
        assert_eq!(args[2], source.to_string_lossy());
    }
}

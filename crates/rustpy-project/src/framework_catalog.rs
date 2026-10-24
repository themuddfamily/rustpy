//! Trusted, cancellable framework metadata. Never called from the render thread.
//! The existing exact-child supervisor owns deadlines, output bounds and reaping.
use rustpy_core::CancellationToken;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Catalog {
    pub installed: Option<Vec<(String, String)>>,
    pub interpreter_valid: Option<bool>,
    pub marker_environment: Vec<(String, String)>,
    pub django_complete: bool,
    pub django_templates: bool,
    pub tags: Vec<String>,
    pub libraries: Vec<(String, Vec<String>, Vec<String>)>,
    pub filters: Vec<String>,
    pub urls: Vec<String>,
    pub settings: Vec<String>,
    pub migrations: Vec<String>,
    pub model_issues: Vec<(String, String, String)>,
    pub web_issues: Vec<(String, usize, String, String)>,
    pub fixtures: Vec<String>,
    pub fixtures_complete: bool,
}
#[derive(Clone, Debug, Hash, PartialEq, Eq)]
struct Key {
    root: PathBuf,
    interpreter: PathBuf,
    revision: u64,
    file: PathBuf,
}
type Cache = HashMap<Key, (Instant, Catalog)>;
static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();
/// A cache miss can execute selected interpreter/project imports, so callers
/// MUST admit project trust first. This API independently refuses untrusted use.
pub fn capture(
    root: &Path,
    interpreter: &Path,
    revision: u64,
    file: &Path,
    trusted: bool,
    cancel: &CancellationToken,
) -> Catalog {
    if !trusted || cancel.is_cancelled() {
        return Catalog::default();
    }
    let key = Key {
        root: root.into(),
        interpreter: interpreter.into(),
        revision,
        file: file.into(),
    };
    let cache = CACHE.get_or_init(Default::default);
    if let Ok(values) = cache.lock() {
        if let Some((at, catalog)) = values.get(&key) {
            if at.elapsed() < Duration::from_secs(30) {
                return catalog.clone();
            }
        }
    }
    static RUNNING: Mutex<()> = Mutex::new(());
    let Ok(_permit) = RUNNING.try_lock() else {
        return Catalog::default();
    };
    let facet = crate::ProjectModel::load(root).frameworks;
    let django = facet.enabled(root);
    let pytest = file.extension().is_some_and(|extension| extension == "py")
        && (root.join("pytest.ini").is_file()
            || root.join("conftest.py").is_file()
            || std::fs::File::open(root.join("pyproject.toml"))
                .ok()
                .and_then(|file| {
                    use std::io::Read;
                    let mut source = String::new();
                    file.take(262145).read_to_string(&mut source).ok()?;
                    (source.len() <= 262144).then_some(source)
                })
                .is_some_and(|source| source.contains("[tool.pytest")));
    let mut catalog = Catalog::default();
    let known = cache.lock().ok().and_then(|values| {
        values
            .iter()
            .find(|(key, (at, catalog))| {
                key.root == root
                    && key.interpreter == interpreter
                    && at.elapsed() < Duration::from_secs(30)
                    && catalog.installed.is_some()
            })
            .map(|(_, (_, catalog))| {
                (
                    catalog.installed.clone(),
                    catalog.interpreter_valid,
                    catalog.marker_environment.clone(),
                )
            })
    });
    let (installed, interpreter_valid, marker_environment) = if let Some(known) = known {
        known
    } else {
        let mut command = crate::interpreter::interpreter_command(interpreter);
        command
            .args(["-I", "-c", PACKAGES])
            .current_dir(root)
            .env("PYTHONDONTWRITEBYTECODE", "1");
        match crate::native_skeleton::supervise_tool(
            &mut command,
            cancel,
            Duration::from_secs(5),
            2 * 1024 * 1024,
        ) {
            Ok((output, _)) => {
                let value = String::from_utf8_lossy(&output)
                    .lines()
                    .rev()
                    .find_map(|line| {
                        line.strip_prefix("RUSTPY_PACKAGES:")
                            .and_then(|value| serde_json::from_str::<Catalog>(value).ok())
                    });
                value
                    .map(|value| {
                        (
                            value.installed,
                            value.interpreter_valid,
                            value.marker_environment,
                        )
                    })
                    .unwrap_or((None, Some(false), Vec::new()))
            }
            Err(_) => (None, None, Vec::new()),
        }
    };

    if django {
        let mut command = crate::interpreter::interpreter_command(interpreter);
        command
            .arg("-c")
            .arg(DJANGO)
            .arg(root)
            .arg(facet.manage(root))
            .envs(facet.environment(root))
            .current_dir(root)
            .env("PYTHONDONTWRITEBYTECODE", "1");
        if let Ok((out, _)) = crate::native_skeleton::supervise_tool(
            &mut command,
            cancel,
            Duration::from_secs(5),
            2 * 1024 * 1024,
        ) {
            if let Some(line) = String::from_utf8_lossy(&out)
                .lines()
                .rev()
                .find_map(|line| line.strip_prefix("RUSTPY_FRAMEWORK:"))
            {
                if let Ok(value) = serde_json::from_str(line) {
                    catalog = value;
                }
            }
        }
    }
    if file.extension().is_some_and(|extension| extension == "py") && !cancel.is_cancelled() {
        // Inspect actual registered framework metadata, never invoke endpoints.
        let mut command = crate::interpreter::interpreter_command(interpreter);
        command
            .args(["-c", WEB])
            .arg(root)
            .arg(file)
            .current_dir(root)
            .env("PYTHONDONTWRITEBYTECODE", "1");
        if let Ok((out, _)) = crate::native_skeleton::supervise_tool(
            &mut command,
            cancel,
            Duration::from_secs(5),
            2 * 1024 * 1024,
        ) {
            if let Some(issues) = String::from_utf8_lossy(&out)
                .lines()
                .rev()
                .find_map(|line| {
                    line.strip_prefix("RUSTPY_WEB:")
                        .and_then(|value| serde_json::from_str(value).ok())
                })
            {
                catalog.web_issues = issues;
            }
        }
    }
    catalog.installed = installed;
    catalog.marker_environment = marker_environment;
    catalog.interpreter_valid = interpreter_valid;
    if pytest && !cancel.is_cancelled() {
        let mut command = crate::interpreter::interpreter_command(interpreter);
        command
            .arg("-m")
            .arg("pytest")
            .args(["--fixtures", "-q", "--color=no"])
            .arg(file)
            .current_dir(root)
            .env("PYTHONDONTWRITEBYTECODE", "1");
        if let Ok((out, _)) = crate::native_skeleton::supervise_tool(
            &mut command,
            cancel,
            Duration::from_secs(5),
            2 * 1024 * 1024,
        ) {
            for line in String::from_utf8_lossy(&out).lines().take(20000) {
                if let Some((head, _)) = line.split_once(" -- ") {
                    let name = head.split_whitespace().next().unwrap_or("");
                    if !name.is_empty()
                        && name.chars().all(|c| c == '_' || c.is_ascii_alphanumeric())
                    {
                        catalog.fixtures.push(name.to_string());
                    }
                }
            }
            catalog.fixtures.sort();
            catalog.fixtures.dedup();
            catalog.fixtures_complete = true;
        }
    }
    if cancel.is_cancelled() {
        return Catalog::default();
    }
    if let Ok(mut values) = cache.lock() {
        values.retain(|_, (at, _)| at.elapsed() < Duration::from_secs(30));
        if values.len() >= 8 {
            values.clear();
        }
        values.insert(key, (Instant::now(), catalog.clone()));
    }
    catalog
}
const DJANGO: &str = r#"
import sys,os,json,ast,pathlib,inspect
root=pathlib.Path(sys.argv[1]);sys.path.insert(0,str(root))
result=dict(django_complete=False,django_templates=False,tags=[],libraries=[],filters=[],urls=[],settings=[],migrations=[],model_issues=[],fixtures=[],fixtures_complete=False)
try:
 source=pathlib.Path(sys.argv[2]).open(encoding='utf-8').read(262145)
 if len(source)>262144:raise ValueError('framework source budget')
 tree=ast.parse(source)
 for node in ast.walk(tree):
  if isinstance(node,ast.Call) and isinstance(node.func,ast.Attribute) and node.func.attr=='setdefault' and len(node.args)>=2:
   if isinstance(node.args[0],ast.Constant) and node.args[0].value=='DJANGO_SETTINGS_MODULE' and isinstance(node.args[1],ast.Constant) and isinstance(node.args[1].value,str):os.environ.setdefault('DJANGO_SETTINGS_MODULE',node.args[1].value)
 import django;django.setup()
 from django.conf import settings
 result['settings']=[n for n in dir(settings) if n.isupper()][:4096]
 from django.template import engines
 engine_list=engines.all()[:16]
 result['django_templates']=bool(engine_list) and all(getattr(engine,'engine',None) is not None for engine in engine_list)
 for engine in engine_list:
  inner=getattr(engine,'engine',None)
  if inner:
   libraries=list(getattr(inner,'template_builtins',[]))
   for name,library in list(getattr(inner,'template_libraries',{}).items())[:256]:result['libraries'].append((name,list(getattr(library,'tags',{}))[:512],list(getattr(library,'filters',{}))[:512]))
   for library in libraries[:256]:
    result['tags'].extend(list(getattr(library,'tags',{}))[:512]);result['filters'].extend(list(getattr(library,'filters',{}))[:512])
 from django.urls import get_resolver
 remaining=[4096]
 def urls(resolver,prefix='',depth=0):
  if depth>16:return
  for item in resolver.url_patterns[:2048]:
   if remaining[0]<=0:return
   remaining[0]-=1
   if getattr(item,'name',None):result['urls'].append(prefix+str(item.name))
   if hasattr(item,'url_patterns'):urls(item,prefix+(str(item.namespace)+':' if getattr(item,'namespace',None) else ''),depth+1)
 urls(get_resolver())
 from django.core.checks import run_checks
 for issue in run_checks(databases=[]):
  obj=getattr(issue,'obj',None);model=getattr(obj,'model',obj)
  try:filename=inspect.getsourcefile(model) or ''
  except (TypeError,OSError):filename=''
  if issue.level>=30:result['model_issues'].append((filename,str(issue.id),str(issue.msg)))
 try:
  from django.db.migrations.loader import MigrationLoader
  from django.db.migrations.state import ProjectState
  from django.db.migrations.autodetector import MigrationAutodetector
  from django.apps import apps
  loader=MigrationLoader(None,ignore_no_migrations=True)
  changes=MigrationAutodetector(loader.project_state(),ProjectState.from_apps(apps)).changes(graph=loader.graph)
  result['migrations']=list(changes)[:1024]
 except Exception:pass
 result['django_complete']=True
except Exception:pass
for key in ['tags','filters','urls','settings']:result[key]=sorted(set(result[key]))[:4096]
print('RUSTPY_FRAMEWORK:'+json.dumps(result))
"#;

/// Queue trusted catalog discovery for non-Python documents (notably templates).
/// A single owned background task avoids render waits and repeated probes.
pub fn request(root: &Path, interpreter: &Path, revision: u64, trusted: bool) {
    if !trusted {
        return;
    }
    let key = Key {
        root: root.into(),
        interpreter: interpreter.into(),
        revision,
        file: root.join("<documents>"),
    };
    if CACHE
        .get()
        .and_then(|cache| cache.try_lock().ok())
        .is_some_and(|cache| {
            cache
                .get(&key)
                .is_some_and(|(at, _)| at.elapsed() < Duration::from_secs(30))
        })
    {
        return;
    }
    static ACTIVE: Mutex<bool> = Mutex::new(false);
    let Ok(mut active) = ACTIVE.try_lock() else {
        return;
    };
    if *active {
        return;
    }
    *active = true;
    drop(active);
    rustpy_core::TaskManager::global()
        .task("Discovering framework metadata")
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
            capture(
                &key.root,
                &key.interpreter,
                key.revision,
                &key.file,
                true,
                progress.token(),
            );
            Ok(())
        });
}

/// Render-thread-safe view of the most recently captured matching project.
pub fn cached(root: &Path, interpreter: &Path, revision: u64) -> Catalog {
    let Some(cache) = CACHE.get() else {
        return Catalog::default();
    };
    let Ok(values) = cache.try_lock() else {
        return Catalog::default();
    };
    values
        .iter()
        .filter(|(key, (at, _))| {
            key.root == root
                && key.interpreter == interpreter
                && key.revision == revision
                && at.elapsed() < Duration::from_secs(30)
        })
        .max_by_key(|(_, (at, _))| *at)
        .map(|(_, (_, catalog))| catalog.clone())
        .unwrap_or_default()
}
const PACKAGES: &str = r#"
import sys,json,itertools,platform,os
version=platform.python_version()
implementation=getattr(sys,'implementation',None)
implementation_version=version
if implementation is not None:
 value=implementation.version;implementation_version='.'.join(str(v) for v in value[:3])
 if value.releaselevel!='final':implementation_version+={'alpha':'a','beta':'b','candidate':'rc'}.get(value.releaselevel,value.releaselevel)+str(value.serial)
environment=dict(implementation_name=getattr(implementation,'name',platform.python_implementation().lower()),implementation_version=implementation_version,os_name=os.name,platform_machine=platform.machine(),platform_python_implementation=platform.python_implementation(),platform_release=platform.release(),platform_system=platform.system(),platform_version=platform.version(),python_full_version=version,python_version='.'.join(version.split('.')[:2]),sys_platform=sys.platform)
result={'interpreter_valid':True,'installed':None,'marker_environment':list(environment.items())}
try:
 import importlib.metadata as metadata
 values=[(str(item.metadata.get('Name','')),str(item.version)) for item in itertools.islice(metadata.distributions(),4097)]
 if len(values)<=4096 and all(len(name)<=200 and len(version)<=100 for name,version in values):result['installed']=values
except Exception:pass
print('RUSTPY_PACKAGES:'+json.dumps(result))
"#;
/// Package installation and explicit cache invalidation retire all observed
/// runtime metadata for this interpreter without retaining stale catalogues.
pub fn invalidate(root: &Path, interpreter: Option<&Path>) {
    if let Some(cache) = CACHE.get() {
        if let Ok(mut values) = cache.lock() {
            values.retain(|key, _| {
                key.root != root
                    || interpreter.is_some_and(|interpreter| key.interpreter != interpreter)
            });
        }
    }
}

const WEB: &str = r#"
import sys,pathlib,ast,importlib,inspect,json
root=pathlib.Path(sys.argv[1]).resolve();path=pathlib.Path(sys.argv[2]).resolve();issues=[]
try:
 source=path.open(encoding='utf-8').read(262145)
 if len(source)>262144:raise ValueError()
 tree=ast.parse(source)
 # A real framework import must be present before executing its trusted module.
 frameworks={'fastapi','flask','sqlalchemy'}
 if not any((isinstance(n,ast.ImportFrom) and (n.module or '').split('.')[0] in frameworks) or (isinstance(n,ast.Import) and any(a.name.split('.')[0] in frameworks for a in n.names)) for n in tree.body):raise ValueError()
 relative=path.relative_to(root).with_suffix('');parts=list(relative.parts)
 if parts[-1]=='__init__':parts.pop()
 if not parts or any(not part.isidentifier() for part in parts):raise ValueError()
 sys.path.insert(0,str(root));module=importlib.import_module('.'.join(parts))
 functions={n.name:n for n in tree.body if isinstance(n,(ast.FunctionDef,ast.AsyncFunctionDef))}
 classes={n.name:n for n in tree.body if isinstance(n,ast.ClassDef)}
 def issue(line,code,message):
  if len(issues)<128:issues.append((str(path),line,code,str(message)[:1000]))
 try:
  from fastapi import FastAPI,APIRouter
  from pydantic import ValidationError
  try:
   from pydantic import TypeAdapter
  except ImportError:TypeAdapter=None
  for app in list(vars(module).values())[:4096]:
   if not isinstance(app,(FastAPI,APIRouter)):continue
   for route in list(app.routes)[:2048]:
    endpoint=getattr(route,'endpoint',None);model=getattr(route,'response_model',None)
    if endpoint is None or model is None or getattr(endpoint,'__module__',None)!=module.__name__:continue
    function=functions.get(getattr(endpoint,'__name__',''))
    if function is None:continue
    # Literal return expressions only: no endpoint execution or guessed body VM.
    pending=list(function.body);steps=0
    while pending and steps<4096:
     node=pending.pop();steps+=1
     if isinstance(node,(ast.FunctionDef,ast.AsyncFunctionDef,ast.ClassDef,ast.Lambda)):continue
     if isinstance(node,ast.Return):
      try:value=ast.literal_eval(node.value)
      except Exception:continue
      try:
       if TypeAdapter is not None:TypeAdapter(model).validate_python(value)
       else:
        from pydantic import parse_obj_as
        parse_obj_as(model,value)
      except ValidationError:issue(node.lineno,'PyRoute','Literal endpoint return does not satisfy its registered response_model')
      except Exception:pass
     else:pending.extend(ast.iter_child_nodes(node))
 except ImportError:pass
 try:
  from flask import Flask
  for app in list(vars(module).values())[:4096]:
   if not isinstance(app,Flask):continue
   for endpoint in list(app.view_functions.values())[:2048]:
    if getattr(endpoint,'__module__',None)!=module.__name__:continue
    function=functions.get(getattr(endpoint,'__name__',''))
    if function is None:continue
    for node in function.body:
     if not isinstance(node,ast.Return):continue
     try:value=ast.literal_eval(node.value)
     except Exception:continue
     if value is None or isinstance(value,(bool,int,float,complex,set)):issue(node.lineno,'PyRoute','Flask view returns a value that cannot form a response')
 except ImportError:pass
 try:
  from sqlalchemy import inspect as sa_inspect
  for name,node in list(classes.items())[:512]:
   cls=getattr(module,name,None)
   if not isinstance(cls,type):continue
   mapper=sa_inspect(cls,raiseerr=False)
   if mapper is None or not hasattr(mapper,'columns'):continue
   try:
    for column in list(mapper.columns)[:1024]:
     default=getattr(column,'default',None)
     value=getattr(default,'arg',None)
     if default is None or callable(value) or value is None:continue
     try:expected=column.type.python_type
     except Exception:continue
     if type(value) in (bool,int,float,str,bytes,list,dict,tuple) and isinstance(expected,type) and not isinstance(value,expected):issue(node.lineno,'PySqlAlchemyModel','Column '+str(column.key)+' default is incompatible with its declared SQL type')
    # Resolving relationships catches real missing names and incompatible maps.
    list(mapper.relationships)
   except Exception as error:issue(node.lineno,'PySqlAlchemyModel','Invalid mapped relationship: '+str(error))
 except ImportError:pass
except Exception:pass
print('RUSTPY_WEB:'+json.dumps(issues))
"#;

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn untrusted_or_cancelled_catalogue_never_starts_selected_interpreter() {
        let token = CancellationToken::new();
        let catalog = capture(
            Path::new("/not-a-project"),
            Path::new("/not-an-interpreter"),
            0,
            Path::new("/not-a-project/test.py"),
            false,
            &token,
        );
        assert!(!catalog.django_complete && !catalog.fixtures_complete);
        token.cancel();
        let catalog = capture(
            Path::new("/not-a-project"),
            Path::new("/not-an-interpreter"),
            0,
            Path::new("/not-a-project/test.py"),
            true,
            &token,
        );
        assert!(!catalog.django_complete && !catalog.fixtures_complete);
    }
}

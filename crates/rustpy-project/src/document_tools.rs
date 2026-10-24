//! Source-bound background validators; no render-thread process waits.
use rustpy_core::{CancellationToken, TaskManager};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    io::Read,
    path::{Path, PathBuf},
    process::Command,
    sync::{
        atomic::{AtomicU64, Ordering},
        Mutex, OnceLock,
    },
    time::{Duration, Instant},
};
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct Key {
    path: PathBuf,
    digest: String,
    interpreter: Option<PathBuf>,
    dialect: Option<String>,
    unused_ignore: bool,
    revision: u64,
}
#[derive(Clone, Debug)]
pub struct Issue {
    pub line: usize,
    pub column: usize,
    pub code: String,
    pub message: String,
}
type Cache = HashMap<Key, (Instant, Vec<Issue>)>;
static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();
static ACTIVE: Mutex<bool> = Mutex::new(false);
static GENERATION: AtomicU64 = AtomicU64::new(0);
pub fn generation() -> u64 {
    GENERATION.load(Ordering::Acquire)
}
fn digest(source: &str) -> String {
    format!("{:x}", Sha256::digest(source.as_bytes()))
}
pub fn inspect(
    source: &str,
    path: &Path,
    interpreter: Option<&Path>,
    dialect: Option<&str>,
    unused_ignore: bool,
    trusted: bool,
    revision: u64,
) -> Vec<Issue> {
    if !trusted || source.len() > 2 * 1024 * 1024 {
        return Vec::new();
    }
    let extension = path.extension().and_then(|s| s.to_str()).unwrap_or("");
    let ignore = unused_ignore && path.file_name().is_some_and(|name| name == ".gitignore");
    if !ignore
        && !matches!(
            extension,
            "sh" | "bash" | "js" | "mjs" | "cjs" | "sql" | "rst"
        )
    {
        return Vec::new();
    }
    let key = Key {
        path: path.into(),
        digest: digest(source),
        interpreter: interpreter.map(Path::to_path_buf),
        dialect: dialect.map(str::to_string),
        unused_ignore,
        revision,
    };
    let cache = CACHE.get_or_init(Default::default);
    if let Ok(cache) = cache.try_lock() {
        if let Some((at, issues)) = cache.get(&key) {
            if at.elapsed() < Duration::from_secs(60) {
                return issues.clone();
            }
        }
    }
    let Ok(mut active) = ACTIVE.try_lock() else {
        return Vec::new();
    };
    if *active {
        return Vec::new();
    }
    *active = true;
    drop(active);
    TaskManager::global()
        .task("Checking document syntax")
        .spawn(move |progress| {
            struct Release;
            impl Drop for Release {
                fn drop(&mut self) {
                    if let Ok(mut active) = ACTIVE.lock() {
                        *active = false;
                    }
                    GENERATION.fetch_add(1, Ordering::AcqRel);
                }
            }
            let _release = Release;
            let issues = run(&key, progress.token());
            if let Ok(mut cache) = cache.lock() {
                if cache.len() >= 16 {
                    cache.clear();
                }
                cache.insert(key, (Instant::now(), issues));
            }
            Ok(())
        });
    Vec::new()
}
fn read(path: &Path) -> Option<String> {
    let mut source = String::new();
    std::fs::File::open(path)
        .ok()?
        .take(2 * 1024 * 1024 + 1)
        .read_to_string(&mut source)
        .ok()?;
    (source.len() <= 2 * 1024 * 1024).then_some(source)
}
fn run(key: &Key, cancel: &CancellationToken) -> Vec<Issue> {
    let Some(source) = read(&key.path).filter(|s| digest(s) == key.digest) else {
        return Vec::new();
    };
    let extension = key.path.extension().and_then(|s| s.to_str()).unwrap_or("");
    let mut command = if key.unused_ignore
        && key
            .path
            .file_name()
            .is_some_and(|name| name == ".gitignore")
    {
        let Ok(git) = which::which("git") else {
            return Vec::new();
        };
        let mut command = Command::new(git);
        command
            .args([
                "ls-files",
                "--others",
                "--ignored",
                "--exclude-standard",
                "-z",
                "--",
                ".",
            ])
            .current_dir(key.path.parent().unwrap_or(Path::new(".")));
        command
    } else {
        match extension {
            "sh" | "bash" => {
                let Ok(program) = which::which(
                    if source
                        .lines()
                        .next()
                        .is_some_and(|l| l.starts_with("#!") && l.ends_with("/sh"))
                    {
                        "sh"
                    } else {
                        "bash"
                    },
                ) else {
                    return Vec::new();
                };
                let mut command = Command::new(program);
                command.arg("-n").arg(&key.path);
                command
            }
            "js" | "mjs" | "cjs" => {
                let Ok(program) = which::which("node") else {
                    return Vec::new();
                };
                let mut command = Command::new(program);
                command
                    .args(["--max-old-space-size=128", "--check"])
                    .arg(&key.path);
                command
            }
            "sql" if key.dialect.is_some() => {
                let Some(program) = &key.interpreter else {
                    return Vec::new();
                };
                let mut command = Command::new(program);
                command
                    .args(["-I", "-c", SQL])
                    .arg(&key.path)
                    .arg(key.dialect.as_deref().unwrap_or("sqlite"));
                command
            }
            "rst" => {
                let Some(program) = &key.interpreter else {
                    return Vec::new();
                };
                let mut command = Command::new(program);
                command.args(["-c", RST]).arg(&key.path);
                command.env("PYTHONDONTWRITEBYTECODE", "1");
                command
            }
            _ => return Vec::new(),
        }
    };
    let Ok((status, out, err)) = crate::native_skeleton::supervise_tool_status(
        &mut command,
        cancel,
        Duration::from_secs(3),
        262144,
    ) else {
        return Vec::new();
    };
    if read(&key.path).is_none_or(|source| digest(&source) != key.digest) {
        return Vec::new();
    }
    if key.unused_ignore
        && key
            .path
            .file_name()
            .is_some_and(|name| name == ".gitignore")
    {
        if !status.success() {
            return Vec::new();
        }
        if out
            .split(|byte| *byte == 0)
            .filter(|name| !name.is_empty())
            .count()
            > 10000
        {
            return Vec::new();
        }
        let parent = key.path.parent().unwrap_or(Path::new("."));
        let mut builder = ignore::gitignore::GitignoreBuilder::new(parent);
        for line in source.lines() {
            if builder.add_line(Some(key.path.clone()), line).is_err() {
                return Vec::new();
            }
        }
        let Ok(matcher) = builder.build() else {
            return Vec::new();
        };
        let mut matched = std::collections::HashSet::new();
        for name in out
            .split(|byte| *byte == 0)
            .filter(|name| !name.is_empty())
            .take(10000)
        {
            let Ok(name) = std::str::from_utf8(name) else {
                continue;
            };
            let result = matcher.matched_path_or_any_parents(parent.join(name), false);
            if let Some(pattern) = result.inner() {
                matched.insert(pattern.original().to_string());
            }
        }
        return source
            .lines()
            .enumerate()
            .filter_map(|(index, line)| {
                let text = line.trim();
                if text.is_empty() || text.starts_with(['#', '!']) || matched.contains(text) {
                    None
                } else {
                    Some(Issue {
                        line: index + 1,
                        column: 1,
                        code: "GitIgnorePattern".into(),
                        message: format!(
                            "Ignore pattern '{text}' does not determine any currently ignored file"
                        ),
                    })
                }
            })
            .take(128)
            .collect();
    }
    if matches!(extension, "sql" | "rst") {
        if let Ok(issues) = serde_json::from_slice::<Vec<(usize, String)>>(&out) {
            return issues
                .into_iter()
                .take(128)
                .map(|(line, message)| Issue {
                    line,
                    column: 1,
                    code: if extension == "rst" {
                        "RstInspection"
                    } else {
                        "SqlDialect"
                    }
                    .into(),
                    message,
                })
                .collect();
        }
        return Vec::new();
    }
    if status.success() {
        return Vec::new();
    }
    let stderr = String::from_utf8_lossy(&err);
    let mut line = 1;
    let mut column = 1;
    if let Some(first) = stderr.lines().next() {
        if let Some(suffix) = first.strip_prefix(key.path.to_str().unwrap_or("")) {
            for word in suffix
                .split(|c: char| !c.is_ascii_digit())
                .filter(|s| !s.is_empty())
                .take(2)
            {
                if let Ok(number) = word.parse::<usize>() {
                    line = number;
                    break;
                }
            }
        }
    }
    if extension == "js" || extension == "mjs" || extension == "cjs" {
        if let Some(caret) = stderr.lines().find(|line| line.trim() == "^") {
            column = caret.find('^').unwrap_or(0) + 1;
        }
    }
    let message = stderr
        .lines()
        .find(|line| {
            line.contains("SyntaxError")
                || line.contains("syntax error")
                || line.contains("unexpected EOF")
                || line.contains("unexpected end of file")
        })
        .map(str::to_string);
    let Some(message) = message else {
        return Vec::new();
    };
    vec![Issue {
        line,
        column,
        code: if extension == "sh" || extension == "bash" {
            "ShellSyntax"
        } else {
            "JavaScriptSyntax"
        }
        .into(),
        message,
    }]
}
const SQL: &str = r#"
import sys,sqlite3,json,pathlib
text=pathlib.Path(sys.argv[1]).open(encoding='utf-8').read(2*1024*1024+1)
if len(text)>2*1024*1024:print('[]');sys.exit(0)
dialect=sys.argv[2]
if dialect!='sqlite':
 try:
  import sqlglot
  from sqlglot.errors import ParseError
 except ImportError:print('[]');sys.exit(0)
 aliases={'postgresql':'postgres','sqlserver':'tsql','mariadb':'mysql'}
 dialect=aliases.get(dialect.lower(),dialect.lower())
 if dialect not in sqlglot.Dialect.classes:print('[]');sys.exit(0)
 try:sqlglot.parse(text,read=dialect,error_level=sqlglot.ErrorLevel.RAISE);issues=[]
 except ParseError as error:issues=[(entry.get('line',1),entry.get('description',str(error))) for entry in error.errors[:128]]
 print(json.dumps(issues));sys.exit(0)
connection=sqlite3.connect(':memory:');issues=[];start=1;statement=''
for line_no,line in enumerate(text.splitlines(True),1):
 statement+=line
 if sqlite3.complete_statement(statement):
  try:connection.execute('EXPLAIN '+statement)
  except sqlite3.Error as error:
   message=str(error)
   if any(word in message.lower() for word in ['syntax error','incomplete input','unrecognized token']):issues.append((start,message))
  statement='';start=line_no+1
if statement.strip():
 try:connection.execute('EXPLAIN '+statement)
 except sqlite3.Error as error:
  message=str(error)
  if any(word in message.lower() for word in ['syntax error','incomplete input','unrecognized token']):issues.append((start,message))
print(json.dumps(issues[:128]))
"#;

// A real Sphinx application loads configured extensions and its cross-document
// environment. It runs only in the admitted trusted-project child, never in UI.
const RST: &str = r#"
import sys,pathlib,tempfile,io,json,re
path=pathlib.Path(sys.argv[1]).resolve();conf=None
for parent in list(path.parents)[:16]:
 if (parent/'conf.py').is_file():conf=parent;break
warnings=io.StringIO();issues=[]
try:
 if conf is not None:
  from sphinx.application import Sphinx
  with tempfile.TemporaryDirectory(prefix='rustpy-sphinx-') as output:
   app=Sphinx(str(conf),str(conf),output,output+'/doctrees','dummy',status=None,warning=warnings,freshenv=True,warningiserror=False,confoverrides={'nitpicky':True})
   app.build(force_all=True)
 else:
  from docutils.core import publish_doctree
  text=path.open(encoding='utf-8').read(2*1024*1024+1)
  if len(text)>2*1024*1024:print('[]');sys.exit(0)
  publish_doctree(text,source_path=str(path),settings_overrides={'warning_stream':warnings,'report_level':2,'halt_level':6,'file_insertion_enabled':False,'raw_enabled':False})
except ImportError:print('[]');sys.exit(0)
except Exception:pass
for line in warnings.getvalue().splitlines()[:4096]:
 # Sphinx's programmatic warning formatter can colour StringIO output too.
 # Remove presentation escapes before resolving the diagnostic's source owner.
 line=re.sub(r'\x1b\[[0-?]*[ -/]*[@-~]','',line)
 match=re.match(r'^(.*?):(\d+):\s*(?:WARNING|ERROR|SEVERE|\((?:WARNING|ERROR|SEVERE)/\d+\))[: ]*(.*)',line)
 if not match:continue
 try:owner=pathlib.Path(match.group(1)).resolve()
 except Exception:continue
 if owner==path:issues.append((int(match.group(2)),match.group(3)))
print(json.dumps(issues[:128]))
"#;

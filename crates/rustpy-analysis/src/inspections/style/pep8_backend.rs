//! Full pycodestyle 2.14.0 fallback. Vendored unmodified MIT source from
//! https://github.com/PyCQA/pycodestyle/tree/2.14.0 (license beside source).
//! The isolated interpreter checks syntax text only; no project module executes.
use super::*;
use std::{
    fs,
    io::Write,
    sync::{
        atomic::{AtomicU64, Ordering},
        Mutex,
    },
    time::Duration,
};
static SERIAL: AtomicU64 = AtomicU64::new(0);
static RUNNING: Mutex<()> = Mutex::new(());
/// Recent backend results by [`cache_key`], newest last. A run spawns an
/// interpreter (hundreds of milliseconds on a large file); re-analysis after
/// a change elsewhere in the project (another file, an index update, an
/// undo) gets the same text and settings and reuses the result.
static RESULTS: Mutex<Vec<(u64, Vec<Problem>)>> = Mutex::new(Vec::new());
const CACHED_RESULTS: usize = 8;

fn cache_key(file: &FileInfo, settings: &StyleInspections, max_length: usize) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    file.text.hash(&mut hasher);
    max_length.hash(&mut hasher);
    settings.pep8_ignore.hash(&mut hasher);
    settings.interpreter.hash(&mut hasher);
    hasher.finish()
}
const CODE: &str = include_str!("../../../resources/pycodestyle/pycodestyle.py");
struct Input(std::path::PathBuf);
impl Drop for Input {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}
pub(super) fn run(
    file: &FileInfo,
    settings: &StyleInspections,
    max_length: usize,
    cancel: &CancellationToken,
) -> Result<Vec<Problem>, String> {
    if file.text.len() > 1024 * 1024 {
        return Err("Full style check exceeds the 1 MiB source budget".into());
    }
    let key = cache_key(file, settings, max_length);
    if let Some((_, problems)) = RESULTS
        .lock()
        .ok()
        .and_then(|results| results.iter().find(|(k, _)| *k == key).cloned())
    {
        return Ok(problems);
    }
    let _guard = RUNNING
        .lock()
        .map_err(|_| "Style checker worker was interrupted")?;
    cancel.check().map_err(|_| "Style check cancelled")?;
    let mut input = None;
    for _ in 0..16 {
        let path = std::env::temp_dir().join(format!(
            "rustpy-style-{}-{}.json",
            std::process::id(),
            SERIAL.fetch_add(1, Ordering::Relaxed)
        ));
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        match options.open(&path) {
            Ok(mut handle) => {
                let owned = Input(path);
                let data=serde_json::to_vec(&serde_json::json!({"source":file.text.as_ref(),"checker":CODE,"max_length":max_length,"ignore":settings.pep8_ignore})).map_err(|error|error.to_string())?;
                handle.write_all(&data).map_err(|error| error.to_string())?;
                input = Some(owned);
                break;
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error.to_string()),
        }
    }
    let input = input.ok_or("Cannot allocate a private style input")?;
    // Run as an actual module so inspect.signature/register_check sees the
    // maintained module globals. No pycodestyle installation/network is needed.
    let script = r#"import json,sys,types
data=json.load(open(sys.argv[1],encoding='utf-8'))
m=types.ModuleType('pycodestyle');m.__file__='<bundled-pycodestyle>';sys.modules[m.__name__]=m
exec(data['checker'],m.__dict__)
m.DEFAULT_IGNORE=''
m.read_config=lambda options,*args:options
class Report(m.BaseReport):
 def __init__(self,options):
  super().__init__(options);self.items=[]
 def error(self,line,offset,text,check):
  code=super().error(line,offset,text,check)
  if code:self.items.append([line,offset,code,text[5:]])
  return code
style=m.StyleGuide(quiet=True,ignore=data['ignore'],max_line_length=data['max_length'],reporter=Report)
checker=m.Checker(lines=data['source'].splitlines(True),options=style.options,report=style.options.reporter(style.options))
checker.check_all()
print(json.dumps(checker.report.items))
"#;
    let interpreter = settings
        .interpreter
        .as_ref()
        .ok_or("Select a Python interpreter for full pycodestyle coverage")?;
    let mut command = rustpy_core::interpreter_launch::interpreter_command(interpreter.as_ref());
    command.args(["-I", "-S", "-c", script]).arg(&input.0);
    let (stdout, _stderr) = rustpy_project::native_skeleton::supervised_tool(
        &mut command,
        cancel,
        Duration::from_secs(3),
        2 * 1024 * 1024,
    )
    .map_err(|error| format!("Full pycodestyle backend unavailable: {error}"))?;
    let problems = decode(file, &stdout)?;
    if let Ok(mut results) = RESULTS.lock() {
        results.retain(|(k, _)| *k != key);
        if results.len() >= CACHED_RESULTS {
            results.remove(0);
        }
        results.push((key, problems.clone()));
    }
    Ok(problems)
}
fn decode(file: &FileInfo, stdout: &[u8]) -> Result<Vec<Problem>, String> {
    let entries: Vec<(usize, usize, String, String)> = serde_json::from_slice(stdout)
        .map_err(|error| format!("Invalid pycodestyle output: {error}"))?;
    let lines: Vec<&str> = file.text.split_inclusive('\n').collect();
    let mut starts = Vec::new();
    let mut offset = 0;
    for line in &lines {
        starts.push(offset);
        offset += line.len()
    }
    let mut out = Vec::new();
    for (line, column, code, message) in entries.into_iter().take(20000) {
        if line == 0
            || code.len() != 4
            || !matches!(code.as_bytes().first(), Some(b'E' | b'W'))
            || !code.as_bytes()[1..].iter().all(u8::is_ascii_digit)
        {
            continue;
        }
        let Some(text) = lines.get(line - 1) else {
            continue;
        };
        let start = starts[line - 1]
            + text
                .char_indices()
                .nth(column)
                .map_or(text.trim_end_matches(['\r', '\n']).len(), |(offset, _)| {
                    offset
                });
        let end = file.text[start..]
            .chars()
            .next()
            .filter(|c| *c != '\n' && *c != '\r')
            .map_or(start, |c| start + c.len_utf8());
        out.push(Problem {
            diagnostic: Diagnostic::new(
                TextRange::new(ts(start), ts(end)),
                Severity::WeakWarning,
                format!("PEP8:{code}"),
                format!("PEP 8: {code} {message}"),
            ),
            fixes: Vec::new(),
        })
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn character_columns_are_translated_to_utf8_ranges_and_bad_records_decline() {
        let db = Analysis::for_project(std::path::Path::new("/project"));
        let id = db.set_file_text(std::path::Path::new("/project/main.py"), "π = 1\n");
        let file = db.file(id).unwrap();
        let problems = decode(
            &file,
            br#"[[1,2,"E225","spacing"],[0,0,"E225","invalid line"],[1,0,"evil","invalid code"]]"#,
        )
        .unwrap();
        assert_eq!(problems.len(), 1);
        assert_eq!(problems[0].diagnostic.range, TextRange::new(ts(3), ts(4)));
        assert!(decode(&file, b"not-json").is_err());
    }
    #[test]
    #[ignore = "final integration: set RUSTPY_TEST_PYTHON to a trusted Python 3.9+ executable"]
    fn bundled_pycodestyle_reports_full_whitespace_and_indentation_codes() {
        let interpreter = std::env::var_os("RUSTPY_TEST_PYTHON").expect("set RUSTPY_TEST_PYTHON");
        let db = Analysis::for_project(std::path::Path::new("/project"));
        let id = db.set_file_text(
            std::path::Path::new("/project/main.py"),
            "def f( value ):\n\treturn value+1  \n",
        );
        let file = db.file(id).unwrap();
        let settings = StyleInspections {
            interpreter: Some(interpreter.into()),
            pep8_ignore: Vec::new(),
            ..Default::default()
        };
        let problems = run(&file, &settings, 79, &CancellationToken::new()).unwrap();
        for code in ["PEP8:E201", "PEP8:E202", "PEP8:W191", "PEP8:W291"] {
            assert!(
                problems
                    .iter()
                    .any(|problem| problem.diagnostic.code == code),
                "missing {code}"
            );
        }
    }
}

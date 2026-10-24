//! Complete bundled style-rule golden corpus. No process is launched unless the
//! central verification stage explicitly enables this ignored suite and supplies
//! RUSTPY_INSPECTION_PYTHON. Source examples retain upstream pycodestyle's rule
//! examples plus dedicated EOF/IO/length/closing-indent cases.
use rustpy_analysis::inspections::infrastructure;
use std::{
    io::Write,
    path::PathBuf,
    process::{Command, Stdio},
};

#[test]
#[ignore = "central verification only: set RUSTPY_INSPECTION_PYTHON to an admitted interpreter"]
fn every_registered_bundled_style_rule_has_a_real_positive_and_clean_control() {
    let executable = PathBuf::from(
        std::env::var_os("RUSTPY_INSPECTION_PYTHON")
            .expect("central verification must select an interpreter explicitly"),
    );
    let cases: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/inspection-contract/pep8.json")).unwrap();
    let directory = tempfile::TempDir::with_prefix("inspection-style-contract").unwrap();
    let payload = serde_json::json!({"checker":include_str!("../resources/pycodestyle/pycodestyle.py"),"cases":cases,"missing":directory.path().join("absent.py")});
    let script = r#"import json,sys,types
payload=json.load(sys.stdin)
checker=types.ModuleType('pycodestyle');checker.__file__='<bundled-pycodestyle>';sys.modules[checker.__name__]=checker
exec(payload['checker'],checker.__dict__)
checker.DEFAULT_IGNORE=''
checker.read_config=lambda options,*args:options
class Report(checker.BaseReport):
 def __init__(self,options):super().__init__(options);self.codes=[]
 def error(self,line,offset,text,check):
  code=super().error(line,offset,text,check)
  if code:self.codes.append(code)
  return code
results=[]
for case in payload['cases']:
 options=checker.StyleGuide(quiet=True,ignore=[],select=[],hang_closing=case['hang_closing'],max_doc_length=case['max_doc_length'],max_line_length=79,reporter=Report).options
 def check(source,missing=False):
  report=Report(options)
  if missing:runner=checker.Checker(filename=payload['missing'],options=options,report=report)
  else:runner=checker.Checker(lines=source.splitlines(True),options=options,report=report)
  runner.check_all();return report.codes
 results.append({'id':case['id'],'bad':check(case['source'],case['missing_file']),'control':check(case['control'])})
json.dump(results,sys.stdout)
"#;
    let mut child = Command::new(&executable)
        .args(["-I", "-S", "-c", script])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("launch explicitly admitted style verification interpreter");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(&serde_json::to_vec(&payload).unwrap())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let results: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    for result in results.as_array().unwrap() {
        let id = result["id"].as_str().unwrap();
        let expected = id.strip_prefix("PEP8:").unwrap();
        let metadata =
            infrastructure::inspection(id).expect("actual checker result needs registry metadata");
        assert!(
            result["bad"]
                .as_array()
                .unwrap()
                .iter()
                .any(|code| code == expected),
            "{id}: positive source did not exercise the actual rule: {result}"
        );
        assert!(
            !result["control"]
                .as_array()
                .unwrap()
                .iter()
                .any(|code| code == expected),
            "{id}: clean control falsely diagnosed: {result}"
        );
        assert!(
            metadata.description_html.contains(expected),
            "{id}: description must identify the actual style obligation"
        );
    }
}

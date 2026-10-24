//! Portable reports and bounded, project-local history. Imports never launch a process.
use super::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Serialize, Deserialize)]
pub(super) struct RunRecord {
    pub timestamp: u64,
    pub last: Option<LastRun>,
    pub tree: Vec<Node>,
    pub results: Vec<TestResult>,
    pub summary: Option<Summary>,
    pub output: Vec<(String, bool)>,
    #[serde(default)]
    pub metadata: Vec<rustpy_test::TestMetadata>,
}
fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}
impl RunRecord {
    pub fn export(&self, format: &str) -> Result<String, String> {
        let json = serde_json::to_string(self)
            .map_err(|error| error.to_string())?
            .replace('<', "\\u003c");
        if format == "html" {
            let mut html = String::from("<!doctype html><html><head><meta charset=\"utf-8\"><title>RustPy test results</title><style>body{font:15px system-ui;max-width:1100px;margin:40px auto}td,th{padding:8px;text-align:left;border-bottom:1px solid #ddd}pre{white-space:pre-wrap}.failed,.error{color:#b42318}.passed{color:#067647}</style></head><body><h1>Test results</h1><table><tr><th>Test</th><th>Outcome</th><th>Duration</th></tr>");
            for result in &self.results {
                html.push_str(&format!(
                    "<tr class=\"{}\"><td>{}</td><td>{}</td><td>{:.6}s</td></tr>",
                    result.outcome.label(),
                    escape(&result.id),
                    result.outcome.label(),
                    result.duration
                ));
                if !result.traceback.is_empty() {
                    html.push_str(&format!(
                        "<tr><td colspan=\"3\"><pre>{}</pre></td></tr>",
                        escape(&result.traceback)
                    ));
                }
                if !result.stdout.is_empty() || !result.stderr.is_empty() {
                    html.push_str(&format!(
                        "<tr><td colspan=\"3\"><pre>{}\n{}</pre></td></tr>",
                        escape(&result.stdout),
                        escape(&result.stderr)
                    ));
                }
            }
            html.push_str(&format!("</table><script id=\"rustpy-results\" type=\"application/json\">{json}</script></body></html>"));
            return Ok(html);
        }
        let failed = self
            .results
            .iter()
            .filter(|result| result.outcome == Outcome::Failed)
            .count();
        let errors = self
            .results
            .iter()
            .filter(|result| result.outcome == Outcome::Error)
            .count();
        let skipped = self
            .results
            .iter()
            .filter(|result| result.outcome == Outcome::Skipped)
            .count();
        let duration: f64 = self.results.iter().map(|result| result.duration).sum();
        let mut xml = format!("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<testsuite name=\"RustPy\" tests=\"{}\" failures=\"{failed}\" errors=\"{errors}\" skipped=\"{skipped}\" time=\"{duration:.6}\">", self.results.len());
        for result in &self.results {
            xml.push_str(&format!(
                "<testcase name=\"{}\" time=\"{:.6}\">",
                escape(&result.id),
                result.duration
            ));
            let status = match result.outcome {
                Outcome::Failed => Some("failure"),
                Outcome::Error => Some("error"),
                Outcome::Skipped => Some("skipped"),
                _ => None,
            };
            if let Some(status) = status {
                xml.push_str(&format!(
                    "<{status} message=\"{}\">{}</{status}>",
                    escape(&result.message),
                    escape(&result.traceback)
                ));
            }
            xml.push_str(&format!(
                "<system-out>{}</system-out><system-err>{}</system-err></testcase>",
                escape(&result.stdout),
                escape(&result.stderr)
            ));
        }
        xml.push_str(&format!(
            "<properties><property name=\"rustpy-report\" value=\"{}\"/></properties></testsuite>",
            escape(&json)
        ));
        Ok(xml)
    }
    pub fn import(text: &str) -> Result<Self, String> {
        if text.len() > 16 * 1024 * 1024 {
            return Err("Test report exceeds 16 MiB".into());
        }
        if let Some((_, rest)) =
            text.split_once("<script id=\"rustpy-results\" type=\"application/json\">")
        {
            let json = rest
                .split_once("</script>")
                .ok_or("Unclosed RustPy report payload")?
                .0;
            return serde_json::from_str(json).map_err(|error| error.to_string());
        }
        let document = roxmltree::Document::parse(text).map_err(|error| error.to_string())?;
        if let Some(payload) = document
            .descendants()
            .find(|node| {
                node.has_tag_name("property") && node.attribute("name") == Some("rustpy-report")
            })
            .and_then(|node| node.attribute("value"))
        {
            return serde_json::from_str(payload).map_err(|error| error.to_string());
        }
        let mut results = Vec::new();
        let mut tree = Vec::new();
        for test in document
            .descendants()
            .filter(|node| node.has_tag_name("testcase"))
        {
            let name = test.attribute("name").unwrap_or("Unnamed test");
            let class = test.attribute("classname").unwrap_or("");
            let id = if class.is_empty() {
                name.to_string()
            } else {
                format!("{class}::{name}")
            };
            let problem = test
                .children()
                .find(|node| matches!(node.tag_name().name(), "failure" | "error" | "skipped"));
            let outcome = match problem.map(|node| node.tag_name().name()) {
                Some("failure") => Outcome::Failed,
                Some("error") => Outcome::Error,
                Some("skipped") => Outcome::Skipped,
                _ => Outcome::Passed,
            };
            let duration = test
                .attribute("time")
                .and_then(|time| time.parse::<f64>().ok())
                .filter(|time| time.is_finite() && *time >= 0.0)
                .unwrap_or(0.0);
            let child_text = |tag| {
                test.children()
                    .find(|node| node.has_tag_name(tag))
                    .and_then(|node| node.text())
                    .unwrap_or("")
                    .to_string()
            };
            tree.push(Node {
                id: id.clone(),
                name: name.into(),
                kind: TestKind::Function,
                path: test.attribute("file").map(PathBuf::from),
                line: test.attribute("line").and_then(|line| line.parse().ok()),
                children: Vec::new(),
            });
            results.push(TestResult {
                id,
                outcome,
                duration,
                message: problem
                    .and_then(|node| node.attribute("message"))
                    .unwrap_or("")
                    .into(),
                traceback: problem.and_then(|node| node.text()).unwrap_or("").into(),
                stdout: child_text("system-out"),
                stderr: child_text("system-err"),
                xfail: false,
                expected: None,
                actual: None,
            });
        }
        if results.is_empty() {
            return Err("No test cases found in the report".into());
        }
        let mut summary = Summary::default();
        for result in &results {
            match result.outcome {
                Outcome::Passed => summary.passed += 1,
                Outcome::Failed => summary.failed += 1,
                Outcome::Error => summary.errors += 1,
                Outcome::Skipped => summary.skipped += 1,
            }
            summary.duration += result.duration;
        }
        summary.exit_code = Some(if summary.failed + summary.errors == 0 {
            0
        } else {
            1
        });
        Ok(Self {
            timestamp: 0,
            last: None,
            tree,
            results,
            summary: Some(summary),
            output: Vec::new(),
            metadata: Vec::new(),
        })
    }
}
pub(super) fn load_history(root: &Path) -> Vec<RunRecord> {
    let path = root.join(".rustpy/testHistory/history.json");
    std::fs::metadata(&path)
        .ok()
        .filter(|metadata| metadata.len() < 32 * 1024 * 1024)
        .and_then(|_| std::fs::read(path).ok())
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}
pub(super) fn save_history(root: &Path, history: &[RunRecord]) -> Result<(), String> {
    let directory = root.join(".rustpy/testHistory");
    std::fs::create_dir_all(&directory).map_err(|error| error.to_string())?;
    let path = directory.join("history.json");
    let stage = path.with_extension("json.tmp");
    std::fs::write(
        &stage,
        serde_json::to_vec(history).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    std::fs::rename(stage, path).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn result() -> TestResult {
        TestResult {
            id: "tests/test_markup.py::test_value[<tag>&\"]".into(),
            outcome: Outcome::Failed,
            duration: 0.125,
            message: "expected <safe> & actual".into(),
            traceback: "File \"tests/test_markup.py\", line 9\nassert '<x>' == '&'".into(),
            stdout: "<script>alert('x')</script>".into(),
            stderr: "warning & details".into(),
            xfail: false,
            expected: Some("<safe>".into()),
            actual: Some("&actual".into()),
        }
    }
    #[test]
    fn portable_html_and_xml_keep_assertion_diffs_and_escape_markup() {
        let result = result();
        let record = RunRecord {
            timestamp: 12,
            last: None,
            tree: Vec::new(),
            results: vec![result.clone()],
            summary: None,
            output: Vec::new(),
            metadata: Vec::new(),
        };
        for format in ["html", "xml"] {
            let exported = record.export(format).unwrap();
            assert!(!exported.contains("<script>alert('x')</script>"));
            let imported = RunRecord::import(&exported).unwrap();
            assert_eq!(imported.results, vec![result.clone()]);
            assert_eq!(
                imported.results[0].comparison(),
                Some(("<safe>", "&actual"))
            );
        }
    }
    #[test]
    fn external_junit_import_maps_failures_errors_skips_and_per_test_output() {
        let record = RunRecord::import(r#"<testsuites><testsuite><testcase classname="suite" name="passed" time="0.2"><system-out>hello</system-out></testcase><testcase name="bad"><failure message="different">trace</failure></testcase><testcase name="oops"><error>setup</error></testcase><testcase name="skip"><skipped message="requires network"/></testcase></testsuite></testsuites>"#).unwrap();
        let summary = record.summary.unwrap();
        assert_eq!(
            (
                summary.passed,
                summary.failed,
                summary.errors,
                summary.skipped
            ),
            (1, 1, 1, 1)
        );
        assert_eq!(record.results[0].stdout, "hello");
        assert_eq!(record.results[1].traceback, "trace");
        assert!(record.last.is_none());
    }
    #[test]
    fn report_import_refuses_nonreports_and_entity_documents() {
        assert!(RunRecord::import("<document>not a report</document>").is_err());
        assert!(RunRecord::import("<!DOCTYPE report [<!ENTITY data SYSTEM 'file:///etc/passwd'>]><testsuite><testcase name='x'><system-out>&data;</system-out></testcase></testsuite>").is_err());
    }
}

//! Portable pstat operations, comparisons and bounded aggregate call layouts.
use crate::profile::{load_json_lines, FunctionStats, ProfileReport, RUNNER_SOURCE};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
fn helper(root: &Path) -> anyhow::Result<PathBuf> {
    let directory = root.join(".rustpy/profiles");
    std::fs::create_dir_all(&directory)?;
    let path = directory.join("profile_tools.py");
    std::fs::write(&path, RUNNER_SOURCE)?;
    Ok(path)
}
pub fn import_report(path: &Path, python: &Path, root: &Path) -> anyhow::Result<ProfileReport> {
    if path
        .extension()
        .is_some_and(|extension| extension == "json")
    {
        return Ok(serde_json::from_slice(&std::fs::read(path)?)?);
    }
    let output = rustpy_project::interpreter::interpreter_command(python)
        .arg(helper(root)?)
        .arg(
            if path
                .extension()
                .is_some_and(|extension| extension == "vmprof")
            {
                "--rustpy-vmprof-import"
            } else {
                "--rustpy-import"
            },
        )
        .arg(path)
        .current_dir(root)
        .stdin(std::process::Stdio::null())
        .output()?;
    anyhow::ensure!(
        output.status.success(),
        "Cannot read pstat: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let mut report = load_json_lines(&String::from_utf8_lossy(&output.stdout))?;
    report.raw_file = Some(path.into());
    if !report.sampling {
        report.pstat_file = Some(path.into());
    }
    Ok(report)
}
pub fn save_report(
    report: &ProfileReport,
    path: &Path,
    python: &Path,
    root: &Path,
) -> anyhow::Result<()> {
    if report.sampling
        && path
            .extension()
            .is_some_and(|extension| extension == "vmprof")
    {
        let original = report
            .raw_file
            .as_ref()
            .filter(|file| file.is_file())
            .ok_or_else(|| {
                anyhow::anyhow!("Raw sampling profile is unavailable; save as .pstat or .json")
            })?;
        if original != path {
            std::fs::copy(original, path)?;
        }
        return Ok(());
    }
    if path
        .extension()
        .is_some_and(|extension| extension == "json")
    {
        std::fs::write(path, serde_json::to_vec_pretty(report)?)?;
        return Ok(());
    }
    if let Some(original) = report
        .pstat_file
        .as_ref()
        .filter(|original| original.is_file())
    {
        if original != path {
            std::fs::copy(original, path)?;
        }
        return Ok(());
    }
    let source = root.join(".rustpy/profiles/export.json");
    let script = helper(root)?;
    std::fs::write(&source, serde_json::to_vec(report)?)?;
    let output = rustpy_project::interpreter::interpreter_command(python)
        .arg(script)
        .arg("--rustpy-save")
        .arg(&source)
        .arg(path)
        .current_dir(root)
        .stdin(std::process::Stdio::null())
        .output()?;
    let _ = std::fs::remove_file(source);
    anyhow::ensure!(
        output.status.success(),
        "Cannot save pstat: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}
#[derive(Clone, Debug, PartialEq)]
pub struct ProfileDelta {
    pub function: FunctionStats,
    pub own_delta: f64,
    pub total_delta: f64,
    pub calls_delta: i64,
}
pub fn compare(current: &ProfileReport, baseline: &ProfileReport) -> Vec<ProfileDelta> {
    let mut values = BTreeMap::<(String, usize, String), (FunctionStats, f64, f64, i128)>::new();
    for function in &current.functions {
        values.insert(
            (function.file.clone(), function.line, function.name.clone()),
            (
                function.clone(),
                function.tottime,
                function.cumtime,
                function.ncalls as i128,
            ),
        );
    }
    for function in &baseline.functions {
        let entry = values
            .entry((function.file.clone(), function.line, function.name.clone()))
            .or_insert_with(|| (function.clone(), 0.0, 0.0, 0));
        entry.1 -= function.tottime;
        entry.2 -= function.cumtime;
        entry.3 -= function.ncalls as i128;
    }
    let mut out: Vec<_> = values
        .into_values()
        .map(|(function, own_delta, total_delta, calls)| ProfileDelta {
            function,
            own_delta,
            total_delta,
            calls_delta: calls.clamp(i64::MIN as i128, i64::MAX as i128) as i64,
        })
        .collect();
    out.sort_by(|a, b| b.own_delta.total_cmp(&a.own_delta));
    out
}
#[derive(Clone, Debug, PartialEq)]
pub struct CallNode {
    pub function: usize,
    pub parent: Option<usize>,
    pub depth: usize,
    pub x: f32,
    pub width: f32,
    pub recursive: bool,
}
pub fn call_tree(
    report: &ProfileReport,
    root: Option<usize>,
    max_depth: usize,
    limit: usize,
) -> Vec<CallNode> {
    fn visit(
        report: &ProfileReport,
        index: usize,
        parent: Option<usize>,
        position: (usize, f32, f32),
        path: &mut Vec<usize>,
        out: &mut Vec<CallNode>,
        bounds: (usize, usize),
    ) {
        let (depth, x, width) = position;
        let (max_depth, limit) = bounds;
        if out.len() >= limit || depth > max_depth || width < 0.00001 {
            return;
        }
        let recursive = path.contains(&index);
        let own = out.len();
        out.push(CallNode {
            function: index,
            parent,
            depth,
            x,
            width,
            recursive,
        });
        if recursive {
            return;
        }
        path.push(index);
        let children = report.callees_of(index);
        let mut weights: Vec<_> = children
            .iter()
            .map(|(child, _)| {
                let parent = &report.functions[index];
                report.functions[*child]
                    .callers
                    .iter()
                    .find(|caller| {
                        caller.file == parent.file
                            && caller.line == parent.line
                            && caller.name == parent.name
                    })
                    .map(|caller| caller.cumtime.max(0.0))
                    .unwrap_or(0.0)
            })
            .collect();
        if weights.iter().all(|weight| *weight == 0.0) {
            for (weight, (_, calls)) in weights.iter_mut().zip(&children) {
                *weight = *calls as f64;
            }
        }
        let total: f64 = weights.iter().sum();
        let mut offset = x;
        for ((child, _), weight) in children.into_iter().zip(weights) {
            let fraction = if total > 0.0 {
                (weight / total) as f32
            } else {
                0.0
            };
            let w = width * fraction;
            visit(
                report,
                child,
                Some(own),
                (depth + 1, offset, w),
                path,
                out,
                bounds,
            );
            offset += w;
        }
        path.pop();
    }
    let roots: Vec<_> = if let Some(root) = root.filter(|root| *root < report.functions.len()) {
        vec![root]
    } else {
        let roots: Vec<_> = report
            .functions
            .iter()
            .enumerate()
            .filter(|(_, function)| function.callers.is_empty())
            .map(|(index, _)| index)
            .collect();
        if roots.is_empty() {
            report.sorted_by_cumtime().into_iter().take(1).collect()
        } else {
            roots
        }
    };
    let mut out = Vec::new();
    let total: f64 = roots
        .iter()
        .map(|root| report.functions[*root].cumtime.max(0.0))
        .sum();
    let mut x = 0.0;
    let root_count = roots.len().max(1) as f32;
    for root in roots {
        let width = if total > 0.0 {
            (report.functions[root].cumtime.max(0.0) / total) as f32
        } else {
            1.0 / root_count
        };
        visit(
            report,
            root,
            None,
            (0, x, width),
            &mut Vec::new(),
            &mut out,
            (max_depth.min(64), limit.min(10000)),
        );
        x += width;
    }
    out
}
fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}
pub fn export_svg(report: &ProfileReport, root: Option<usize>, path: &Path) -> anyhow::Result<()> {
    let nodes = call_tree(report, root, 20, 2000);
    let height = (nodes.iter().map(|node| node.depth).max().unwrap_or(0) + 1) * 36;
    let mut svg = format!(
        "<svg xmlns='http://www.w3.org/2000/svg' width='1200' height='{height}' viewBox='0 0 1200 {height}'>"
    );
    for (node_id, node) in nodes.into_iter().enumerate() {
        let function = &report.functions[node.function];
        let ratio = if report.total_time > 0.0 {
            (function.tottime / report.total_time).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let green = (190.0 * (1.0 - ratio)) as u8;
        let x = node.x * 1200.0;
        let width = node.width * 1200.0;
        let y = node.depth * 36;
        svg.push_str(&format!("<g><title>{} — {:.6}s own / {:.6}s total</title><rect x='{x}' y='{y}' width='{width}' height='32' fill='rgb(245,{green},80)' stroke='white'/><clipPath id='n{}'><rect x='{x}' y='{y}' width='{width}' height='32'/></clipPath><text x='{}' y='{}' font-size='12' clip-path='url(#n{})'>{}{}</text></g>",escape(&function.display_name()),function.tottime,function.cumtime,node_id,x+3.0,y+21,node_id,escape(&function.name),if node.recursive{" [recursive]"}else{""}));
    }
    svg.push_str("</svg>");
    std::fs::write(path, svg)?;
    Ok(())
}

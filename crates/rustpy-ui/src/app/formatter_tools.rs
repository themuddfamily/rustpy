//! External formatters always consume an immutable stdin snapshot. They never
//! receive an on-disk input filename to rewrite; publication checks ownership.
use super::*;
use rustpy_core::{TaskHandle, TaskManager};
use rustpy_format::candidate::{
    external_command_output, formatter_arguments, parse_arguments, FormatterLimits,
};
use std::process::Command;

pub(super) struct Job {
    tab: egui::Id,
    document: rustpy_text::DocumentId,
    version: u64,
    path: Option<PathBuf>,
    project: Option<Arc<Analysis>>,
    interpreter: Option<PathBuf>,
    settings: rustpy_settings::CodeStyleSettings,
    diagnostics: bool,
    handle: TaskHandle<String>,
}

impl Drop for Job {
    fn drop(&mut self) {
        self.handle.cancel();
    }
}

pub(super) fn matches_masks(masks: &str, path: &Path, root: Option<&Path>) -> bool {
    let relative = root
        .and_then(|root| path.strip_prefix(root).ok())
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/");
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    masks
        .split([',', ';'])
        .take(128)
        .map(str::trim)
        .filter(|mask| !mask.is_empty())
        .any(|mask| glob(mask, &name) || glob(mask, &relative))
}
fn glob(pattern: &str, text: &str) -> bool {
    if pattern.len() > 4096 || text.len() > 4096 {
        return false;
    }
    let pattern: Vec<_> = pattern.chars().collect();
    let text: Vec<_> = text.chars().collect();
    let (mut p, mut t, mut star, mut retry) = (0, 0, None, 0);
    let mut work = 0;
    while t < text.len() {
        work += 1;
        if work > 131072 {
            return false;
        }
        if p < pattern.len() && (pattern[p] == '?' || pattern[p] == text[t]) {
            p += 1;
            t += 1;
        } else if p < pattern.len() && pattern[p] == '*' {
            star = Some(p);
            p += 1;
            retry = t;
        } else if let Some(saved) = star {
            retry += 1;
            t = retry;
            p = saved + 1;
        } else {
            return false;
        }
    }
    while p < pattern.len() && pattern[p] == '*' {
        p += 1;
    }
    p == pattern.len()
}

impl IdeApp {
    pub(super) fn selected_formatter(&self) -> &str {
        if self.settings.code_style.use_black_default {
            "black"
        } else {
            &self.settings.code_style.formatter
        }
    }
    pub(super) fn external_format_enabled(&self, index: usize, on_save: bool) -> bool {
        let tool = self.selected_formatter();
        self.settings
            .code_style
            .external_formatters
            .iter()
            .find(|config| config.tool == tool)
            .is_none_or(|config| {
                let path = self.tabs[index]
                    .state
                    .path
                    .as_deref()
                    .unwrap_or_else(|| Path::new("untitled.py"));
                (if on_save {
                    config.on_save
                } else {
                    config.on_reformat
                }) && matches_masks(
                    &config.file_masks,
                    path,
                    self.project.as_ref().map(|p| p.root.as_path()),
                )
            })
    }
    pub(super) fn external_formatter_command(
        &self,
        index: usize,
        tool: &str,
        diagnostics: bool,
    ) -> anyhow::Result<Command> {
        self.external_formatter_command_for_path(
            self.tabs[index].state.path.as_deref(),
            tool,
            diagnostics,
        )
    }
    pub(super) fn external_formatter_command_for_path(
        &self,
        path: Option<&Path>,
        tool: &str,
        diagnostics: bool,
    ) -> anyhow::Result<Command> {
        let project_safe_mode = self.project_safe_mode();
        if project_safe_mode {
            anyhow::bail!("trust the project before running an external formatter");
        }
        let interpreter = self.project.as_ref().and_then(|p| p.interpreter.as_ref());
        let config = self
            .settings
            .code_style
            .external_formatters
            .iter()
            .find(|c| c.tool == tool);
        let executable = config
            .map(|c| c.executable.trim())
            .filter(|s| !s.is_empty());
        let python = interpreter
            .map(|i| i.path.clone())
            .unwrap_or_else(|| PathBuf::from("python3"));
        let mut command = rustpy_core::interpreter_launch::interpreter_command(
            executable.map(Path::new).unwrap_or(&python),
        );
        if executable.is_none() {
            command.args(["-m", tool]);
        }
        if diagnostics {
            command.args([
                "check",
                "--output-format",
                "json",
                "--no-fix",
                "--no-cache",
                "-",
            ]);
            if let Some(path) = path {
                command.arg("--stdin-filename").arg(path);
            }
        } else {
            command.args(formatter_arguments(tool, path)?);
        }
        let arguments = config
            .map(|c| c.arguments.as_str())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| {
                if tool == self.settings.code_style.formatter {
                    self.settings.code_style.formatter_args.as_str()
                } else {
                    ""
                }
            });
        let extra = parse_arguments(arguments)?;
        validate_arguments(&extra)?;
        command.args(extra);
        let inherited = std::env::var("PATH").unwrap_or_default();
        command.envs(rustpy_project::env::venv_env(interpreter, &inherited));
        if let Some(project) = &self.project {
            command.current_dir(&project.root);
            let inherited = std::env::var("PYTHONPATH").ok();
            if let Some(value) = rustpy_project::env::pythonpath_for(
                &project.root,
                &project.tree.model(),
                true,
                true,
                inherited.as_deref(),
            ) {
                command.env("PYTHONPATH", value);
            }
        } else if let Some(parent) = path.and_then(Path::parent) {
            command.current_dir(parent);
        }
        Ok(command)
    }
    pub(super) fn start_external_formatter(&mut self, tool: &str, diagnostics: bool) {
        let Some(index) = self.active.filter(|&i| i < self.tabs.len()) else {
            return;
        };
        if self.tabs[index].state.language != Language::Python {
            self.set_status("External Python formatters require a Python file");
            return;
        }
        let config = self
            .settings
            .code_style
            .external_formatters
            .iter()
            .find(|c| c.tool == tool);
        if config.is_some_and(|c| {
            !matches_masks(
                &c.file_masks,
                self.tabs[index]
                    .state
                    .path
                    .as_deref()
                    .unwrap_or_else(|| Path::new("untitled.py")),
                self.project.as_ref().map(|p| p.root.as_path()),
            )
        }) {
            self.set_status("File excluded by formatter masks");
            return;
        }
        if self.tabs[index].state.doc.is_read_only() && !diagnostics {
            self.set_status("File is read-only");
            return;
        }
        let command = match self.external_formatter_command(index, tool, diagnostics) {
            Ok(c) => c,
            Err(e) => {
                self.set_status(e.to_string());
                return;
            }
        };
        if !diagnostics {
            let style = &self.settings.code_style;
            let path = self.tabs[index]
                .state
                .path
                .as_deref()
                .unwrap_or_else(|| Path::new("."));
            if self.code_style_excludes(path).unwrap_or(true) {
                self.set_status("File excluded by Code Style do-not-format scopes");
                return;
            }
            if style.python != rustpy_settings::python_style::PythonStyle::default()
                || style.reformat_scope != "all"
                || style.formatter_markers
                || style.keep_line_breaks
                || style.keep_simple_blocks
                || style.keep_first_column_comments
            {
                self.set_status("Changed-text/keep/marker policy requires the builtin formatter");
                return;
            }
            let path = self.tabs[index]
                .state
                .path
                .as_deref()
                .unwrap_or_else(|| Path::new("."));
            let ec = self.settings.editor.editorconfig_for(path);
            if ec.ide.formatter_enabled == Some(false) {
                self.set_status("Formatting disabled by EditorConfig");
                return;
            }
            if super::indent_status::imported_formatter_tags(&ec)
                .is_some_and(|tags| tags.has_start(&self.tabs[index].state.doc.text()))
            {
                self.set_status("Configured protected regions require the builtin formatter");
                return;
            }
        }
        drop(self.bg.reformat.take());
        if let Some(old) = self.bg.external_formatter.take() {
            old.handle.cancel();
        }
        let tab = &self.tabs[index];
        let text = tab.state.doc.text();
        let handle = TaskManager::global()
            .task(if diagnostics {
                "Ruff diagnostics"
            } else {
                "External formatter"
            })
            .spawn(move |progress| {
                let bytes = external_command_output(
                    &text,
                    command,
                    FormatterLimits::default(),
                    diagnostics,
                    progress.token(),
                )
                .map_err(|e| match e {
                    rustpy_format::candidate::CandidateError::Cancelled => {
                        rustpy_core::TaskError::Cancelled
                    }
                    other => rustpy_core::TaskError::Failed(other.to_string()),
                })?;
                let output = String::from_utf8(bytes)
                    .map_err(|e| rustpy_core::TaskError::Failed(e.to_string()))?;
                if !diagnostics && output.is_empty() && !text.is_empty() {
                    return Err("formatter returned empty output".into());
                }
                Ok(output)
            });
        self.bg.external_formatter = Some(Job {
            tab: tab.view.id,
            document: tab.state.doc.identity(),
            version: tab.state.doc.version(),
            path: tab.state.path.clone(),
            project: self.project.as_ref().map(|p| p.analysis.clone()),
            interpreter: self
                .project
                .as_ref()
                .and_then(|p| p.interpreter.as_ref())
                .map(|i| i.path.clone()),
            settings: self.settings.code_style.clone(),
            diagnostics,
            handle,
        });
        self.set_status(format!("Running {tool}; cancel in Background Tasks"));
    }
    pub(super) fn poll_external_formatter(&mut self) {
        let project_safe_mode = self.project_safe_mode();
        let stale = self.bg.external_formatter.as_ref().is_some_and(|job| {
            let tab = self.tabs.iter().find(|tab| tab.view.id == job.tab);
            let project = match (&job.project, self.project.as_ref()) {
                (Some(old), Some(current)) => Arc::ptr_eq(old, &current.analysis),
                (None, None) => true,
                _ => false,
            };
            let interpreter = self
                .project
                .as_ref()
                .and_then(|p| p.interpreter.as_ref())
                .map(|i| i.path.clone());
            project_safe_mode
                || !project
                || interpreter != job.interpreter
                || self.settings.code_style != job.settings
                || tab.is_none_or(|tab| {
                    tab.state.doc.identity() != job.document
                        || tab.state.doc.version() != job.version
                        || tab.state.path != job.path
                })
        });
        if stale {
            drop(self.bg.external_formatter.take());
            self.set_status("Discarded stale external formatter result");
            return;
        }
        let Some(job) = self.bg.external_formatter.as_mut() else {
            return;
        };
        let Some(result) = job.handle.try_result() else {
            return;
        };
        let job = self.bg.external_formatter.take().unwrap();
        let index = self.tabs.iter().position(|tab| {
            tab.view.id == job.tab
                && tab.state.doc.identity() == job.document
                && tab.state.doc.version() == job.version
                && tab.state.path == job.path
        });
        let project = self.project.as_ref().map(|p| &p.analysis);
        let same_project = match (&job.project, project) {
            (Some(old), Some(now)) => Arc::ptr_eq(old, now),
            (None, None) => true,
            _ => false,
        };
        let interpreter = self
            .project
            .as_ref()
            .and_then(|p| p.interpreter.as_ref())
            .map(|i| i.path.clone());
        if project_safe_mode
            || !same_project
            || interpreter != job.interpreter
            || self.settings.code_style != job.settings
            || index.is_none()
        {
            self.set_status("Discarded stale external formatter result");
            return;
        }
        let index = index.unwrap();
        if job.handle.is_cancelled() {
            self.set_status("External formatter cancelled");
            return;
        }
        if !job.diagnostics && self.tabs[index].state.doc.is_read_only() {
            self.set_status("Discarded formatter result: file is read-only");
            return;
        }
        match result {
            Ok(text) if job.diagnostics => {
                match ruff_diagnostics(&text, &self.tabs[index].state.doc.text()) {
                    Ok(diagnostics) => {
                        let count = diagnostics.len();
                        self.tabs[index].state.external_diagnostics =
                            Some((job.version, diagnostics));
                        self.set_status(format!("Ruff: {count} diagnostic(s)"));
                    }
                    Err(error) => self.set_status(format!("Invalid Ruff diagnostics: {error}")),
                }
            }
            Ok(text) => {
                let text = rustpy_text::LineEnding::normalize_to_lf(&text).into_owned();
                let state = &mut self.tabs[index].state;
                let lines =
                    crate::editor::save_actions::changed_lines(&state.doc.text(), &text).len();
                let edits = document_markers::refine_formatter_edits(
                    &state.doc,
                    &[(TextRange::new(TextSize::new(0), state.doc.len()), text)],
                );
                state.apply_text_edits(
                    edits.into_iter().map(|e| (e.range, e.text)).collect(),
                    "External formatter",
                );
                self.set_status(format!("{lines} lines reformatted by external formatter"));
            }
            Err(error) => self.set_status(format!("External formatter: {error}")),
        }
    }
}

fn validate_arguments(arguments: &[String]) -> anyhow::Result<()> {
    let values = [
        "--line-length",
        "-l",
        "--target-version",
        "-t",
        "--config",
        "--profile",
        "--settings-path",
        "--settings-file",
        "--style",
        "--indent-size",
        "--max-line-length",
        "--ignore",
        "--select",
        "--extend-ignore",
        "--extend-select",
        "--skip",
        "--skip-glob",
        "--known-first-party",
        "--known-third-party",
        "--src-path",
        "--multi-line",
        "--lines",
        "--range",
    ];
    let flags = [
        "--quiet",
        "-q",
        "--verbose",
        "-v",
        "--fast",
        "--safe",
        "--preview",
        "--unstable",
        "--skip-string-normalization",
        "-S",
        "--skip-magic-trailing-comma",
        "-C",
        "--skip-source-first-line",
        "--float-to-top",
        "--atomic",
        "--force-single-line-imports",
        "--combine-as",
        "--aggressive",
        "-a",
        "--experimental",
        "--ignore-local-config",
        "--no-cache",
    ];
    let mut i = 0;
    while i < arguments.len() {
        let argument = &arguments[i];
        if flags.contains(&argument.as_str()) {
            i += 1;
            continue;
        }
        let (name, inline) = argument
            .split_once('=')
            .map(|(a, b)| (a, Some(b)))
            .unwrap_or((argument, None));
        if !values.contains(&name) {
            anyhow::bail!("unsupported stdin-safe formatter argument: {argument}");
        }
        if inline.is_none() {
            i += 1;
            if arguments.get(i).is_none_or(|value| value.starts_with('-')) {
                anyhow::bail!("missing value for formatter argument {name}");
            }
        }
        i += 1;
    }
    Ok(())
}

fn ruff_diagnostics(json: &str, source: &str) -> anyhow::Result<Vec<rustpy_core::Diagnostic>> {
    let values: Vec<serde_json::Value> = serde_json::from_str(json)?;
    if values.len() > 4096 {
        anyhow::bail!("too many diagnostics");
    }
    let mut lines: Vec<_> = source
        .split_inclusive('\n')
        .scan(0usize, |at, line| {
            let start = *at;
            *at += line.len();
            Some((start, line))
        })
        .collect();
    if source.is_empty() || source.ends_with('\n') {
        lines.push((source.len(), ""));
    }
    let offset = |position: &serde_json::Value| -> Option<TextSize> {
        let row = usize::try_from(position["row"].as_u64()?.checked_sub(1)?).ok()?;
        let column = usize::try_from(position["column"].as_u64()?.checked_sub(1)?).ok()?;
        let (start, line) = lines.get(row)?;
        let byte = line
            .char_indices()
            .map(|(i, _)| i)
            .chain(std::iter::once(line.len()))
            .nth(column)?;
        Some(TextSize::new(u32::try_from(start + byte).ok()?))
    };
    let mut result = Vec::new();
    for value in values {
        let Some(start) = offset(&value["location"]) else {
            anyhow::bail!("invalid location");
        };
        let Some(end) = offset(&value["end_location"]) else {
            anyhow::bail!("invalid end location");
        };
        if end < start {
            anyhow::bail!("reversed range");
        }
        let code = value["code"].as_str().unwrap_or("syntax");
        let message = value["message"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("missing message"))?;
        result.push(rustpy_core::Diagnostic::warning(
            TextRange::new(start, end),
            &format!("Ruff/{code}"),
            message,
        ));
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn masks_quoted_options_and_ruff_unicode_coordinates_preserve_protocol() {
        assert!(matches_masks("*.py; *.pyi", Path::new("src/α.pyi"), None));
        assert!(!matches_masks("src/*.py", Path::new("tests/α.py"), None));
        let args =
            parse_arguments("--config 'folder with spaces/config.toml' --line-length=88").unwrap();
        validate_arguments(&args).unwrap();
        for bad in [
            "--in-place",
            "other.py",
            "--check",
            "--diff",
            "--output=result.py",
            "-- --fix",
        ] {
            assert!(
                validate_arguments(&parse_arguments(bad).unwrap()).is_err(),
                "{bad}"
            );
        }
        let diagnostics=ruff_diagnostics(r#"[{"code":"F401","message":"unused","location":{"row":1,"column":1},"end_location":{"row":1,"column":2}}]"#,"π = 1\n").unwrap();
        assert_eq!(diagnostics[0].range, TextRange::new(0.into(), 2.into()));
        assert_eq!(diagnostics[0].code.as_str(), "Ruff/F401");
        assert!(ruff_diagnostics(r#"[{"location":{"row":9,"column":1}}]"#, "x\n").is_err());
    }
    #[cfg(unix)]
    fn fixture(script: &str) -> (tempfile::TempDir, IdeApp, PathBuf) {
        use std::os::unix::fs::PermissionsExt;
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("m.py");
        std::fs::write(&path, "x=1\n").unwrap();
        let executable = directory.path().join("formatter with spaces");
        std::fs::write(&executable, format!("#!/bin/sh\n{script}\n")).unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
        let mut settings = Settings::default();
        settings.trust_project(directory.path());
        let mut app = IdeApp::new(settings, Some(directory.path().to_owned()));
        app.open_file(&path);
        for config in &mut app.settings.code_style.external_formatters {
            config.executable = executable.to_string_lossy().into_owned();
        }
        (directory, app, path)
    }
    #[cfg(unix)]
    fn finish(app: &mut IdeApp) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while app.bg.external_formatter.is_some() {
            app.poll_external_formatter();
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }
    #[cfg(unix)]
    const REAL_TOOLS: [&str; 5] = ["black", "ruff", "isort", "autopep8", "yapf"];

    #[cfg(unix)]
    fn configured_executable(variable: &str) -> PathBuf {
        let path = PathBuf::from(
            std::env::var_os(variable)
                .unwrap_or_else(|| panic!("configured acceptance requires {variable}")),
        );
        assert!(
            path.is_absolute() && path.is_file(),
            "{variable} must name an installed executable: {}",
            path.display()
        );
        path
    }

    #[cfg(unix)]
    fn real_fixture(
        tool: &str,
        module: bool,
    ) -> (
        tempfile::TempDir,
        IdeApp,
        PathBuf,
        &'static str,
        &'static str,
    ) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("m.py");
        let (source, expected) = if tool == "isort" {
            ("import sys\nimport os\n", "import os\nimport sys\n")
        } else {
            ("x=1\n", "x = 1\n")
        };
        std::fs::write(&path, source).unwrap();
        let mut settings = Settings::default();
        settings.trust_project(directory.path());
        let config = settings
            .code_style
            .external_formatters
            .iter_mut()
            .find(|config| config.tool == tool)
            .unwrap();
        config.executable = if module {
            String::new()
        } else {
            configured_executable(&format!("RUSTPY_TEST_{}", tool.to_ascii_uppercase()))
                .to_string_lossy()
                .into_owned()
        };
        config.arguments = match tool {
            "black" | "ruff" => "--line-length 88",
            "isort" => "--profile black",
            "autopep8" => "--max-line-length 88",
            "yapf" => "--style pep8",
            _ => unreachable!(),
        }
        .into();
        config.on_save = true;
        config.on_reformat = true;
        settings.code_style.formatter = tool.into();
        let mut app = IdeApp::new(settings, Some(directory.path().to_owned()));
        if module {
            let python = configured_executable("RUSTPY_TEST_FORMATTER_PYTHON");
            app.project.as_mut().unwrap().interpreter = Some(rustpy_project::Interpreter {
                prefix: python.parent().unwrap().parent().unwrap().to_path_buf(),
                path: python,
                version: String::new(),
                sys_path: Vec::new(),
                is_venv: true,
                platform: std::env::consts::OS.into(),
                kind: "configured formatter acceptance".into(),
            });
        }
        app.open_file(&path);
        (directory, app, path, source, expected)
    }

    #[cfg(unix)]
    fn accept_real_formatters(module: bool) {
        for tool in REAL_TOOLS {
            let (_directory, mut app, path, source, expected) = real_fixture(tool, module);
            let index = app.active.unwrap();
            // Actual Reformat admission and async publication, not a direct peer invocation.
            app.reformat_code();
            assert!(
                app.bg.external_formatter.is_some(),
                "{tool}: {:?}",
                app.status
            );
            finish(&mut app);
            assert_eq!(
                app.tabs[index].state.doc.text(),
                expected,
                "{tool}: {:?}",
                app.status
            );
            assert_eq!(std::fs::read_to_string(&path).unwrap(), source);
            assert!(
                app.tabs[index].state.undo(),
                "{tool} must publish one undoable edit"
            );
            assert_eq!(app.tabs[index].state.doc.text(), source);
            assert!(
                !app.tabs[index].state.undo(),
                "{tool} must not publish multiple undo units"
            );
            app.settings.code_style.format_on_save = true;
            assert!(app.save_tab(index), "{tool}: {:?}", app.status);
            assert_eq!(app.tabs[index].state.doc.text(), expected);
            assert_eq!(
                std::fs::read_to_string(&path).unwrap(),
                expected,
                "{tool} save must publish actual provider output"
            );
            // Changing the captured source before polling must never publish old output.
            app.start_external_formatter(tool, false);
            assert!(app.bg.external_formatter.is_some());
            app.tabs[index]
                .state
                .doc
                .set_text("changed=2\n", &[], &[])
                .unwrap();
            finish(&mut app);
            assert_eq!(app.tabs[index].state.doc.text(), "changed=2\n");
            assert_eq!(std::fs::read_to_string(&path).unwrap(), expected);
            app.start_external_formatter(tool, false);
            app.bg
                .external_formatter
                .as_ref()
                .expect("configured provider admitted")
                .handle
                .cancel();
            finish(&mut app);
            assert_eq!(
                app.tabs[index].state.doc.text(),
                "changed=2\n",
                "cancelled {tool} cannot publish"
            );
            app.settings
                .code_style
                .external_formatters
                .iter_mut()
                .find(|config| config.tool == tool)
                .unwrap()
                .file_masks = "*.txt".into();
            app.start_external_formatter(tool, false);
            assert!(
                app.bg.external_formatter.is_none(),
                "mask exclusion must refuse before launch"
            );
            app.safe_mode = true;
            assert!(
                app.external_formatter_command(index, tool, false).is_err(),
                "{tool} requires trust even when explicitly configured"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    #[ignore = "configured acceptance: real RUSTPY_TEST_BLACK/RUFF/ISORT/AUTOPEP8/YAPF executables required"]
    fn installed_formatter_executables_reformat_save_undo_and_refuse_stale_results() {
        accept_real_formatters(false);
    }

    #[cfg(unix)]
    #[test]
    #[ignore = "configured acceptance: RUSTPY_TEST_FORMATTER_PYTHON must have all five real formatter modules installed"]
    fn installed_formatter_modules_reformat_save_undo_and_refuse_stale_results() {
        accept_real_formatters(true);
    }

    #[cfg(unix)]
    #[test]
    #[ignore = "configured acceptance: real RUSTPY_TEST_RUFF executable required"]
    fn installed_ruff_check_imports_real_diagnostics_without_changing_source() {
        let (_directory, mut app, path, _, _) = real_fixture("ruff", false);
        let source = "import os\n";
        let index = app.active.unwrap();
        app.tabs[index]
            .state
            .doc
            .set_text(source, &[], &[])
            .unwrap();
        app.settings
            .code_style
            .external_formatters
            .iter_mut()
            .find(|config| config.tool == "ruff")
            .unwrap()
            .arguments = "--select F401".into();
        app.start_external_formatter("ruff", true);
        assert!(app.bg.external_formatter.is_some());
        finish(&mut app);
        assert!(
            app.tabs[index]
                .state
                .diagnostics()
                .iter()
                .any(|diagnostic| diagnostic.code == "Ruff/F401"),
            "{:?}",
            app.status
        );
        assert_eq!(app.tabs[index].state.doc.text(), source);
        assert_eq!(std::fs::read_to_string(path).unwrap(), "x=1\n");
    }

    #[cfg(unix)]
    #[test]
    fn all_five_tools_apply_snapshot_once_and_undo_without_writing_disk() {
        for tool in ["black", "ruff", "isort", "autopep8", "yapf"] {
            let (_directory, mut app, path) = fixture("sed 's/x=1/x = 1/'");
            app.start_external_formatter(tool, false);
            finish(&mut app);
            assert_eq!(
                app.tabs[0].state.doc.text(),
                "x = 1\n",
                "{tool}: {:?}",
                app.status
            );
            assert_eq!(std::fs::read_to_string(path).unwrap(), "x=1\n");
            assert!(app.tabs[0].state.undo());
            assert_eq!(app.tabs[0].state.doc.text(), "x=1\n");
        }
    }
    #[cfg(unix)]
    #[test]
    fn reformat_and_save_triggers_masks_and_black_default_use_same_runner() {
        let (_directory, mut app, path) = fixture("sed 's/x=1/x = 1/'");
        app.settings.code_style.use_black_default = true;
        app.settings
            .code_style
            .external_formatters
            .iter_mut()
            .find(|c| c.tool == "black")
            .unwrap()
            .on_reformat = false;
        app.reformat_code();
        assert!(app.bg.external_formatter.is_none());
        app.settings.code_style.format_on_save = true;
        assert!(app.save_tab(0), "{:?}", app.status);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "x = 1\n");
        app.tabs[0].state.doc.set_text("x=1\n", &[], &[]).unwrap();
        app.settings
            .code_style
            .external_formatters
            .iter_mut()
            .find(|c| c.tool == "black")
            .unwrap()
            .file_masks = "*.txt".into();
        assert!(app.save_tab(0));
        assert_eq!(std::fs::read_to_string(path).unwrap(), "x=1\n");
    }
    #[cfg(unix)]
    #[test]
    fn ruff_exit_one_imports_diagnostics_but_edit_discards_stale_worker_output() {
        let (_directory, mut app, _) = fixture(
            "cat >/dev/null; printf '%s' '[{\"code\":\"F401\",\"message\":\"unused\",\"location\":{\"row\":1,\"column\":1},\"end_location\":{\"row\":1,\"column\":2}}]'; exit 1",
        );
        app.start_external_formatter("ruff", true);
        finish(&mut app);
        assert!(app.tabs[0]
            .state
            .diagnostics()
            .iter()
            .any(|d| d.code == "Ruff/F401"));
        app.start_external_formatter("ruff", true);
        app.tabs[0]
            .state
            .doc
            .set_text("changed\n", &[], &[])
            .unwrap();
        finish(&mut app);
        assert!(!app.tabs[0]
            .state
            .diagnostics()
            .iter()
            .any(|d| d.code == "Ruff/F401"));
        assert!(app.status.as_ref().unwrap().0.contains("stale"));
        app.safe_mode = true;
        app.start_external_formatter("black", false);
        assert!(app.bg.external_formatter.is_none());
    }
}

//! Save actions produce one isolated candidate; no tool receives --write/--fix.
use super::*;
use rustpy_settings::save_actions::WebTool;
use std::process::Command;

fn tool_command(
    name: &str,
    options: &WebTool,
    path: &Path,
    root: &Path,
) -> anyhow::Result<Command> {
    let local = root.join("node_modules/.bin").join(if cfg!(windows) {
        format!("{name}.cmd")
    } else {
        name.into()
    });
    let program = if !options.executable.trim().is_empty() {
        PathBuf::from(&options.executable)
    } else if local.is_file() {
        local
    } else {
        PathBuf::from(name)
    };
    let mut command = rustpy_core::interpreter_launch::interpreter_command(&program);
    command.current_dir(root);
    let args = rustpy_format::candidate::parse_arguments(&options.arguments)?;
    // The save transaction owns publication; tool options cannot opt into an
    // independent write of the source file while a candidate is prepared.
    if args.iter().any(|arg| {
        matches!(arg.as_str(), "--write" | "-w" | "--fix")
            || arg.starts_with("--write=")
            || arg.starts_with("--fix=")
    }) {
        anyhow::bail!(
            "Save actions use stdin candidates; remove --write/--fix from {name} arguments"
        );
    }
    command.args(args);
    if name == "prettier" {
        // https://prettier.io/docs/cli#--stdin-filepath
        command.arg("--stdin-filepath").arg(path);
    } else {
        // https://eslint.org/docs/latest/use/command-line-interface#--fix-dry-run
        command
            .args([
                "--stdin",
                "--fix-dry-run",
                "--format",
                "json",
                "--stdin-filename",
            ])
            .arg(path);
    }
    Ok(command)
}
fn eslint_candidate(source: &str, bytes: &[u8]) -> anyhow::Result<String> {
    let rows: Vec<serde_json::Value> = serde_json::from_slice(bytes)?;
    if rows.len() != 1 {
        anyhow::bail!("ESLint returned {} results for one input", rows.len());
    }
    let row = &rows[0];
    if row
        .get("fatalErrorCount")
        .and_then(|v| v.as_u64())
        .unwrap_or(0)
        > 0
    {
        anyhow::bail!(
            "ESLint could not parse the save candidate: {}",
            row.get("messages").unwrap_or(&serde_json::Value::Null)
        );
    }
    match row.get("output") {
        None | Some(serde_json::Value::Null) => Ok(source.into()),
        Some(serde_json::Value::String(output)) => Ok(output.clone()),
        _ => anyhow::bail!("ESLint output must be text"),
    }
}
impl IdeApp {
    pub(super) fn save_actions_apply(&self, path: &Path) -> bool {
        (!self.save_synchronization.automatic || self.settings.save_actions.on_auto_save)
            && super::formatter_tools::matches_masks(
                &self.settings.save_actions.masks,
                path,
                self.project.as_ref().map(|project| project.root.as_path()),
            )
    }
    pub(super) fn prepare_web_save_candidate(
        &self,
        path: &Path,
        source: &str,
    ) -> anyhow::Result<String> {
        let project_safe_mode = self.project_safe_mode();
        let root = self
            .project
            .as_ref()
            .map(|project| project.root.as_path())
            .or_else(|| path.parent())
            .unwrap_or(Path::new("."));
        let mut candidate = source.to_owned();
        let token = rustpy_core::CancellationToken::new();
        for (name, options) in [
            ("prettier", &self.settings.save_actions.prettier),
            ("eslint", &self.settings.save_actions.eslint),
        ] {
            if !options.enabled
                || !super::formatter_tools::matches_masks(&options.masks, path, Some(root))
            {
                continue;
            }
            if project_safe_mode {
                anyhow::bail!("Trust the project before running {name} on save");
            }
            let command = tool_command(name, options, path, root)?;
            let output = rustpy_format::candidate::external_command_output(
                &candidate,
                command,
                rustpy_format::candidate::FormatterLimits::default(),
                name == "eslint",
                &token,
            )?;
            candidate = if name == "eslint" {
                eslint_candidate(&candidate, &output)?
            } else {
                let output = String::from_utf8(output)?;
                if !candidate.is_empty() && output.is_empty() {
                    anyhow::bail!("Prettier returned empty output");
                }
                output
            };
            candidate = rustpy_text::LineEnding::normalize_to_lf(&candidate).into_owned();
        }
        Ok(candidate)
    }
    pub(super) fn prepare_save_copyright(
        &self,
        path: &Path,
        source: &str,
    ) -> anyhow::Result<Option<rustpy_text::Edit>> {
        use rustpy_settings::copyright::{self, CopyrightProfile, Variables};
        if !self.settings.save_actions.update_copyright
            || !self.settings.copyright.enabled
            || !copyright::supports_comments(path)
        {
            return Ok(None);
        }
        let legacy = CopyrightProfile::from_legacy(&self.settings.copyright.text);
        let profile = if self.settings.copyright_profiles.is_unconfigured() {
            Some(&legacy)
        } else {
            self.settings
                .copyright_profiles
                .validate()
                .map_err(anyhow::Error::msg)?;
            self.settings
                .copyright_profiles
                .profile_for(
                    path,
                    self.project.as_ref().map(|project| project.root.as_path()),
                )
                .map_err(anyhow::Error::msg)?
        };
        let Some(profile) = profile else {
            return Ok(None);
        };
        let now = chrono::Local::now();
        let year = now.format("%Y").to_string();
        let date = now.format("%Y-%m-%d").to_string();
        let project = self
            .project
            .as_ref()
            .and_then(|project| project.root.file_name())
            .unwrap_or_default()
            .to_string_lossy();
        let user = std::env::var("USER")
            .or_else(|_| std::env::var("USERNAME"))
            .unwrap_or_else(|_| "user".into());
        let file = path.file_name().unwrap_or_default().to_string_lossy();
        let vars = Variables {
            year: &year,
            date: &date,
            project: &project,
            user: &user,
            file: &file,
        };
        let edit =
            copyright::plan_update(source, path, profile, &vars).map_err(anyhow::Error::msg)?;
        Ok(edit.map(|edit| {
            rustpy_text::Edit::replace(
                TextRange::new(
                    TextSize::new(edit.start as u32),
                    TextSize::new(edit.end as u32),
                ),
                edit.replacement,
            )
        }))
    }
    pub(super) fn save_action_settings(&mut self, ui: &mut egui::Ui) {
        ui.collapsing("Actions on Save", |ui| {
            let actions = &mut self.settings.save_actions;
            ui.horizontal(|ui| { ui.label("File masks (; separated)"); ui.text_edit_singleline(&mut actions.masks); });
            ui.checkbox(&mut actions.on_auto_save, "Run actions during automatic saves");
            ui.checkbox(&mut actions.update_copyright, "Update copyright using configured profiles");
            ui.checkbox(&mut actions.run_file_watchers, "Run enabled File Watchers after changes are saved");
            ui.checkbox(&mut actions.upload, "Upload to default deployment server when automatic upload is enabled");
            for (name, tool) in [("Prettier", &mut actions.prettier), ("ESLint fix", &mut actions.eslint)] {
                ui.group(|ui| {
                    ui.checkbox(&mut tool.enabled, format!("Run {name} on save"));
                    ui.horizontal(|ui| { ui.label("Executable (empty: project tool or PATH)"); ui.text_edit_singleline(&mut tool.executable); });
                    ui.horizontal(|ui| { ui.label("Arguments"); ui.text_edit_singleline(&mut tool.arguments); });
                    ui.horizontal(|ui| { ui.label("File masks"); ui.text_edit_singleline(&mut tool.masks); });
                });
            }
            ui.label("Reformat / Optimize imports / Rearrange / Cleanup use the code-style controls above. Black, Ruff and isort use the selected formatter's save trigger and masks. Auto-save timing is configured in Saving and Synchronization.");
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (tempfile::TempDir, IdeApp, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("source.py");
        std::fs::write(&path, "#!/bin/python\n# coding: utf-8\nvalue=1\n").unwrap();
        let mut settings = Settings::default();
        settings.copyright.enabled = true;
        settings.copyright.text = "Copyright $today.year Acme".into();
        settings.save_actions.update_copyright = true;
        let mut app = IdeApp::new_welcome(settings);
        app.open_file(&path);
        (dir, app, path)
    }
    #[test]
    fn save_copyright_preserves_python_preamble_and_publishes_one_undo_unit() {
        let (_dir, mut app, path) = fixture();
        let before = app.tabs[0].state.doc.text();
        assert!(app.save_tab(0));
        let saved = std::fs::read_to_string(&path).unwrap();
        assert!(saved.starts_with("#!/bin/python\n# coding: utf-8\n"));
        assert!(saved.contains("Copyright"));
        assert!(saved.ends_with("value=1\n"));
        assert_eq!(app.tabs[0].state.doc.text(), saved);
        assert!(!app.tabs[0].state.doc.is_modified());
        assert!(app.tabs[0].state.doc.undo().is_some());
        assert_eq!(app.tabs[0].state.doc.text(), before);
    }
    #[test]
    fn failed_external_stage_does_not_publish_prior_copyright_candidate_or_touch_disk() {
        let (dir, mut app, path) = fixture();
        app.safe_mode = false;
        app.settings.save_actions.prettier.enabled = true;
        app.settings.save_actions.prettier.masks = "*.py".into();
        app.settings.save_actions.prettier.executable =
            dir.path().join("missing-formatter").display().to_string();
        let before = app.tabs[0].state.doc.text();
        let version = app.tabs[0].state.doc.version();
        let history = format!("{:?}", app.tabs[0].state.doc.undo_history());
        assert!(!app.save_tab(0));
        assert_eq!(app.tabs[0].state.doc.text(), before);
        assert_eq!(app.tabs[0].state.doc.version(), version);
        assert_eq!(
            format!("{:?}", app.tabs[0].state.doc.undo_history()),
            history
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
    }
    #[test]
    fn masks_and_automatic_save_policy_control_actual_prepared_save_actions() {
        let (_dir, mut app, path) = fixture();
        let original = std::fs::read_to_string(&path).unwrap();
        app.settings.save_actions.on_auto_save = false;
        app.save_synchronization.automatic = true;
        assert!(app.save_tab(0));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
        app.save_synchronization.automatic = false;
        app.settings.save_actions.masks = "*.js".into();
        assert!(app.save_tab(0));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
        app.settings.save_actions.masks = "*.py".into();
        assert!(app.save_tab(0));
        assert!(std::fs::read_to_string(&path)
            .unwrap()
            .contains("Copyright"));
    }
    #[test]
    fn eslint_retains_unfixed_source_applies_returned_fix_and_rejects_fatal_or_multiple_results() {
        assert_eq!(
            eslint_candidate("x", br#"[{"fatalErrorCount":0,"messages":[]}]"#).unwrap(),
            "x"
        );
        assert_eq!(
            eslint_candidate(
                "x",
                br#"[{"fatalErrorCount":0,"output":"fixed","messages":[]}]"#
            )
            .unwrap(),
            "fixed"
        );
        assert!(eslint_candidate(
            "x",
            br#"[{"fatalErrorCount":1,"messages":[{"fatal":true}]}]"#
        )
        .is_err());
        assert!(eslint_candidate("x", b"[]").is_err());
        assert!(eslint_candidate("x", b"not-json").is_err());
    }
    #[test]
    fn web_tool_receives_whole_filename_and_cannot_request_its_own_source_write() {
        let mut options = WebTool {
            executable: "configured-prettier".into(),
            arguments: "--tab-width '4'".into(),
            ..Default::default()
        };
        let path = Path::new("/project with spaces/my file.js");
        let command = tool_command(
            "prettier",
            &options,
            path,
            Path::new("/project with spaces"),
        )
        .unwrap();
        assert_eq!(command.get_args().last(), Some(path.as_os_str()));
        options.arguments = "--write".into();
        assert!(tool_command("prettier", &options, path, Path::new(".")).is_err());
        options.arguments = "--fix".into();
        assert!(tool_command("eslint", &options, path, Path::new(".")).is_err());
    }
}

#[cfg(test)]
mod configured_provider_tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn required(name: &str) -> PathBuf {
        let path =
            PathBuf::from(std::env::var_os(name).unwrap_or_else(|| panic!("required {name}")));
        assert!(
            path.is_absolute() && path.is_file(),
            "{name} requires an actual absolute provider path"
        );
        path
    }
    fn arg(path: &Path) -> String {
        rustpy_deploy::exec::sh_quote(&path.to_string_lossy())
    }
    fn literal(path: &Path) -> String {
        serde_json::to_string(&path.to_string_lossy()).unwrap()
    }
    fn logging(log: &Path, provider: &str) -> String {
        format!(
            "const stat=fs.readFileSync('/proc/self/stat','utf8');const start=stat.slice(stat.lastIndexOf(')')+1).trim().split(/\\s+/)[19];fs.appendFileSync({},JSON.stringify({{provider:{},pid:process.pid,start,cwd:process.cwd()}})+'\\n');",
            literal(log),
            serde_json::to_string(provider).unwrap()
        )
    }
    fn receipts(log: &Path) -> Vec<serde_json::Value> {
        std::fs::read_to_string(log)
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }
    fn assert_retired(log: &Path, project: &Path) {
        for receipt in receipts(log) {
            assert_eq!(receipt["cwd"], project.to_string_lossy().as_ref());
            let pid = receipt["pid"].as_u64().unwrap();
            let captured = receipt["start"].as_str().unwrap();
            if let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) {
                let now = stat
                    .rsplit_once(')')
                    .unwrap()
                    .1
                    .split_whitespace()
                    .nth(19)
                    .unwrap();
                assert_ne!(
                    now, captured,
                    "actual Node provider must physically retire, not merely publish output"
                );
            }
        }
    }
    fn options(node: &Path, cli: &Path, config: &Path, eslint: bool) -> WebTool {
        WebTool {
            enabled: true,
            executable: node.to_string_lossy().into_owned(),
            arguments: format!(
                "{} --config {}{}",
                arg(cli),
                arg(config),
                if eslint { " --no-config-lookup" } else { "" }
            ),
            masks: "*.js".into(),
        }
    }

    #[test]
    #[ignore = "requires explicit RUSTPY_WEB_SAVE_ADMITTED=1, NODE/PRETTIER_CLI/ESLINT_CLI paths and private TEST_ROOT; Prettier 3 and ESLint 9, Linux"]
    fn configured_prettier_eslint_and_watcher_publish_real_checked_save_candidates() {
        std::fs::read_to_string("/proc/self/stat")
            .expect("configured provider identity acceptance requires live Linux /proc; never silently skips");
        assert_eq!(
            std::env::var("RUSTPY_WEB_SAVE_ADMITTED").as_deref(),
            Ok("1")
        );
        let node = required("RUSTPY_TEST_NODE");
        let prettier = required("RUSTPY_TEST_PRETTIER_CLI");
        let eslint = required("RUSTPY_TEST_ESLINT_CLI");
        let admitted = PathBuf::from(
            std::env::var_os("RUSTPY_WEB_SAVE_TEST_ROOT")
                .expect("private existing test root required"),
        );
        assert!(admitted.is_absolute() && admitted.is_dir());
        let root = tempfile::tempdir_in(admitted).unwrap();
        let root = root.path();
        let log = root.join("actual-provider-receipts.jsonl");
        let prettier_config = root.join("prettier.config.cjs");
        std::fs::write(
            &prettier_config,
            format!(
                "const fs=require('node:fs');{}module.exports={{singleQuote:true,semi:false}};",
                logging(&log, "prettier")
            ),
        )
        .unwrap();
        let eslint_config = root.join("eslint.config.mjs");
        std::fs::write(&eslint_config, format!("import fs from 'node:fs';{}export default [{{files:['**/*.js'],languageOptions:{{ecmaVersion:2022,sourceType:'module'}},rules:{{quotes:['error','double'],semi:['error','always'],'no-var':'error','prefer-const':'error'}}}}];", logging(&log,"eslint"))).unwrap();
        let source = "var value={x:'hello'};console.log(value)\n";
        let expected = "const value = { x: \"hello\" };\nconsole.log(value);\n";
        let path = root.join("actual source.js");
        std::fs::write(&path, source).unwrap();
        let mut settings = Settings::default();
        settings.trust_project(root);
        settings.save_actions.prettier = options(&node, &prettier, &prettier_config, false);
        settings.save_actions.eslint = options(&node, &eslint, &eslint_config, true);
        settings.save_actions.run_file_watchers = false;
        settings.save_actions.upload = false;
        let mut app = IdeApp::new(settings, Some(root.into()));
        app.open_file(&path);
        let tab = app.active.unwrap();
        assert!(app.save_tab(tab), "{:?}", app.status);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), expected);
        assert_eq!(app.tabs[tab].state.doc.text(), expected);
        assert!(!app.tabs[tab].state.doc.is_modified());
        let initial = receipts(&log);
        assert_eq!(
            initial
                .iter()
                .map(|r| r["provider"].as_str().unwrap())
                .collect::<Vec<_>>(),
            ["prettier", "eslint"],
            "both installed providers actually run in save order"
        );
        assert_retired(&log, root);
        assert!(app.tabs[tab].state.doc.undo().is_some());
        assert_eq!(
            app.tabs[tab].state.doc.text(),
            source,
            "combined candidate publication is one undoable save edit"
        );

        // Dirty read-only source and safe mode must refuse before either real
        // provider config executes. The config receipt is an invocation witness.
        app.safe_mode = true;
        let version = app.tabs[tab].state.doc.version();
        assert!(!app.save_tab(tab));
        assert_eq!(receipts(&log).len(), initial.len());
        assert_eq!(app.tabs[tab].state.doc.version(), version);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), expected);
        app.safe_mode = false;
        app.dialog = Dialog::None;
        let original_permissions = std::fs::metadata(&path).unwrap().permissions();
        let mut readonly = original_permissions.clone();
        readonly.set_readonly(true);
        std::fs::set_permissions(&path, readonly).unwrap();
        assert!(!app.save_tab(tab));
        assert_eq!(receipts(&log).len(), initial.len());
        assert_eq!(app.tabs[tab].state.doc.version(), version);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), expected);
        std::fs::set_permissions(&path, original_permissions).unwrap();
        app.dialog = Dialog::None;

        // A genuine Prettier config provides a bounded coordination barrier,
        // not a formatter shim. Another writer changes disk after the App has
        // captured its baseline but before the real candidate is returned.
        let stale_path = root.join("stale source.js");
        std::fs::write(&stale_path, source).unwrap();
        app.open_file(&stale_path);
        let stale_tab = app.active.unwrap();
        let marker = root.join("provider-entered");
        let release = root.join("provider-release");
        let barrier_config = root.join("barrier.prettier.cjs");
        std::fs::write(&barrier_config, format!("const fs=require('node:fs');{}fs.writeFileSync({},'entered');const until=Date.now()+10000;while(!fs.existsSync({})){{if(Date.now()>until)throw new Error('fixture barrier timed out');Atomics.wait(new Int32Array(new SharedArrayBuffer(4)),0,0,10);}}module.exports={{singleQuote:true,semi:false}};",logging(&log,"prettier-stale"),literal(&marker),literal(&release))).unwrap();
        app.settings.save_actions.prettier = options(&node, &prettier, &barrier_config, false);
        app.settings.save_actions.eslint.enabled = false;
        let stale_version = app.tabs[stale_tab].state.doc.version();
        let external = "// an independent writer changed this file\n";
        std::thread::scope(|scope| {
            let writer = scope.spawn(|| -> Result<(), String> {
                let deadline = Instant::now() + Duration::from_secs(8);
                while !marker.exists() {
                    if Instant::now() >= deadline {
                        return Err("actual Prettier never loaded admitted config".into());
                    }
                    std::thread::sleep(Duration::from_millis(5));
                }
                let written = std::fs::write(&stale_path, external).map_err(|e| e.to_string());
                let released = std::fs::write(&release, "release").map_err(|e| e.to_string());
                written.and(released)
            });
            let saved = app.save_tab(stale_tab);
            writer.join().unwrap().unwrap();
            assert!(
                !saved,
                "stale disk baseline must reject real formatted candidate"
            );
        });
        assert_eq!(app.tabs[stale_tab].state.doc.version(), stale_version);
        assert_eq!(app.tabs[stale_tab].state.doc.text(), source);
        assert_eq!(std::fs::read_to_string(&stale_path).unwrap(), external);
        assert_retired(&log, root);

        // Actual successful save queues an installed Prettier watcher. Its
        // physical RunSession completes before reconciliation publishes disk.
        app.settings.save_actions.prettier.enabled = false;
        app.settings.save_actions.run_file_watchers = true;
        app.settings
            .file_watchers
            .push(rustpy_settings::FileWatcher {
                name: "Actual installed Prettier".into(),
                enabled: true,
                pattern: "watch*.js".into(),
                program: node.to_string_lossy().into_owned(),
                arguments: format!(
                    "{} --config {} --write $FilePath$",
                    arg(&prettier),
                    arg(&prettier_config)
                ),
                output_paths: "$FilePath$".into(),
                debounce_ms: 0,
                ..Default::default()
            });
        let watched = root.join("watch actual.js");
        std::fs::write(&watched, "// disk baseline\n").unwrap();
        app.open_file(&watched);
        let watched_tab = app.active.unwrap();
        app.tabs[watched_tab]
            .state
            .doc
            .set_text("const watch={π:'hello'}\n", &[], &[])
            .unwrap();
        app.dialog = Dialog::None;
        assert!(app.save_tab(watched_tab));
        assert!(
            !app.file_watcher_pending_labels().is_empty(),
            "actual changed save queues watcher"
        );
        let ctx = egui::Context::default();
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            app.run_panel.poll();
            app.file_watchers_tick(&ctx);
            app.reconcile_external_changes();
            let sessions = app
                .run_panel
                .sessions
                .iter()
                .filter(|s| s.name == "File watcher: Actual installed Prettier")
                .collect::<Vec<_>>();
            if let Some(session) = sessions.first() {
                if !session.is_running()
                    && app.tabs[watched_tab].state.doc.text() == "const watch = { π: 'hello' }\n"
                {
                    assert_eq!(sessions.len(), 1);
                    assert_eq!(session.status, rustpy_run::SessionStatus::Exited(0));
                    break;
                }
            }
            if Instant::now() >= deadline {
                let pending = app.file_watcher_pending_labels();
                app.stop_file_watchers();
                let sessions = app
                    .run_panel
                    .sessions
                    .iter()
                    .map(|session| {
                        (
                            &session.name,
                            &session.command_line,
                            &session.status,
                            session
                                .output
                                .iter()
                                .map(|chunk| chunk.text.as_str())
                                .collect::<Vec<_>>(),
                        )
                    })
                    .collect::<Vec<_>>();
                panic!(
                    "actual installed watcher did not finish/reconcile: status={:?}; pending={:?}; sessions={sessions:?}; disk={:?}; editor={:?}; modified={}",
                    app.status,
                    pending,
                    std::fs::read_to_string(&watched),
                    app.tabs[watched_tab].state.doc.text(),
                    app.tabs[watched_tab].state.doc.is_modified()
                );
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(
            std::fs::read_to_string(&watched).unwrap(),
            "const watch = { π: 'hello' }\n"
        );
        assert!(!app.tabs[watched_tab].state.doc.is_modified());
        assert_eq!(
            receipts(&log)
                .iter()
                .map(|r| r["provider"].as_str().unwrap())
                .collect::<Vec<_>>(),
            ["prettier", "eslint", "prettier-stale", "prettier"]
        );
        assert_retired(&log, root);
    }
}

//! Data-document commands reuse shared buffers, schema snapshots and tool runners.
use super::*;
use crate::a11y::DialogWindow;
use rustpy_core::{TaskHandle, TaskManager};
use rustpy_project::data_formats;
#[derive(Default)]
pub(super) struct State {
    owner: Option<(egui::Id, rustpy_text::DocumentId, u64)>,
    path: PathBuf,
    source: String,
    outline: Vec<crate::editor::syntax_support::StructureItem>,
    requirement: Option<(String, String)>,
    pub open: bool,
    tab: usize,
    filter: String,
    xpath: String,
    schema: String,
    preview: String,
    message: String,
    archive: Option<crate::data_archive::Archive>,
    archive_job: Option<TaskHandle<crate::data_archive::Archive>>,
    patch_job: Option<TaskHandle<PathBuf>>,
    retired: bool,
}
impl State {
    #[cfg(all(test, target_os = "linux"))]
    pub(super) fn acceptance_details(&self) -> String {
        format!(
            "message={:?}; owner={:?}; selected_tab={}",
            self.message, self.owner, self.tab
        )
    }
    pub fn cancel(&self) {
        if let Some(job) = &self.archive_job {
            job.cancel();
        }
        if let Some(job) = &self.patch_job {
            job.cancel();
        }
    }
    pub fn busy(&self) -> bool {
        self.archive_job.is_some() || self.patch_job.is_some()
    }
    pub fn stamp(&self) -> Vec<u64> {
        self.archive_job
            .iter()
            .map(TaskHandle::id)
            .chain(self.patch_job.iter().map(TaskHandle::id))
            .collect()
    }
}
impl Drop for State {
    fn drop(&mut self) {
        self.cancel();
    }
}
#[derive(Clone, Copy)]
enum Command {
    Format,
    Validate,
    JsonPath,
    Table,
    Schema,
    XmlSchema,
    Dtd,
    XPath,
    GenerateXml,
    ShellCheck,
    Shfmt,
    Taplo,
    Prettier,
    Preview,
    Patch,
    InstallRequirement,
    UpgradeRequirement,
    Run,
    Plugin(&'static str),
}
impl IdeApp {
    pub(super) fn cancel_data_files_project(&mut self) {
        self.data_files.cancel();
        self.data_files.retired = true;
        self.data_files.owner = None;
        self.data_files.open = false;
        self.data_files.archive = None;
        self.data_files.requirement = None;
        if matches!(self.dialog, Dialog::DataFiles) {
            self.dialog = Dialog::None;
        }
    }
    pub(super) fn open_data_files(&mut self) {
        if self.data_files.busy() {
            self.set_status("Wait for the current data-file operation");
            return;
        }
        let Some(index) = self.active else { return };
        self.data_files.retired = false;
        let tab = &self.tabs[index];
        if tab.state.is_large_file() || usize::from(tab.state.doc.len()) > 2 * 1024 * 1024 {
            self.set_status(
                "Structured tools are limited to 2 MiB; large-file source remains available",
            );
            return;
        }
        self.data_files.owner = Some((
            tab.view.id,
            tab.state.doc.identity(),
            tab.state.doc.version(),
        ));
        self.data_files.path = tab
            .state
            .path
            .clone()
            .unwrap_or_else(|| PathBuf::from("untitled.json"));
        self.data_files.source = tab.state.doc.text();
        self.data_files.outline =
            crate::editor::data_documents::outline(&self.data_files.source, &self.data_files.path);
        self.data_files.archive = None;
        self.data_files.requirement = None;
        self.data_files.preview.clear();
        self.data_files.message.clear();
        self.data_files.open = true;
        self.dialog = Dialog::DataFiles;
    }
    pub(super) fn open_data_archive(&mut self, path: &Path) {
        if self.data_files.busy() {
            self.set_status("Wait for the active data-file task");
            return;
        }
        self.data_files.owner = None;
        self.data_files.retired = false;
        self.data_files.path = path.to_path_buf();
        self.data_files.source.clear();
        self.data_files.preview.clear();
        self.data_files.archive = None;
        self.data_files.open = true;
        let path = path.to_path_buf();
        let maximum = self.settings.data_files.archive_limit_mib.clamp(1, 256) * 1024 * 1024;
        self.data_files.archive_job = Some(
            TaskManager::global()
                .task("Opening ZIP archive")
                .spawn(move |progress| {
                    progress.check()?;
                    let archive = crate::data_archive::Archive::open(&path, maximum)
                        .map_err(rustpy_core::TaskError::Failed)?;
                    progress.check()?;
                    Ok(archive)
                }),
        );
        self.dialog = Dialog::DataFiles;
    }
    pub(super) fn poll_data_files(&mut self, ctx: &egui::Context) {
        if let Some(result) = self
            .data_files
            .archive_job
            .as_mut()
            .and_then(TaskHandle::try_result)
        {
            self.data_files.archive_job = None;
            match result {
                Ok(archive) if !self.data_files.retired => self.data_files.archive = Some(archive),
                Err(error) if !self.data_files.retired => {
                    self.data_files.message = error.to_string()
                }
                _ => {}
            }
        }
        if let Some(result) = self
            .data_files
            .patch_job
            .as_mut()
            .and_then(TaskHandle::try_result)
        {
            self.data_files.patch_job = None;
            match result {
                Ok(root) if !self.data_files.retired => {
                    self.data_files.message = format!("Patch applied to {}", root.display());
                    if let Some(project) = &mut self.project {
                        if project.git.as_ref().is_some_and(|repo| repo.root() == root) {
                            project.git_refreshed = -100.0;
                        }
                    }
                    self.refresh_project_files();
                }
                Err(error) if !self.data_files.retired => {
                    self.data_files.message = error.to_string()
                }
                _ => {}
            }
        }
        if self.data_files.busy() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
    }
    pub(super) fn data_files_toolbar(&mut self, ui: &mut egui::Ui, index: usize) {
        let language = self.tabs[index].state.language;
        if !matches!(
            language,
            Language::Python
                | Language::PlainText
                | Language::Other
                | Language::Markdown
                | Language::Gherkin
                | Language::Rst
        ) {
            ui.horizontal(|ui| {
                if ui.small_button("Data file tools…").clicked() {
                    self.active = Some(index);
                    self.open_data_files();
                }
            });
        }
    }
    pub(super) fn data_files_dialog(&mut self, ctx: &egui::Context) {
        self.dialog = Dialog::DataFiles;
        let mut open = true;
        let mut command = None;
        let mut pick = None;
        let mut jump = None;
        egui::Window::new(format!("Data tools — {}",self.data_files.path.display()))
            .id(egui::Id::new("data_file_tools")).open(&mut open).resizable(true).default_width(900.0)
            .show_modal(ctx,|ui| {
                if let Some((name,_))=&self.data_files.requirement {
                    ui.label(format!("Requirement: {name}"));
                    if ui.button("Install requirement").clicked(){command=Some(Command::InstallRequirement);}
                    if ui.button("Upgrade package").clicked(){command=Some(Command::UpgradeRequirement);}
                }
                if let Some(archive)=&self.data_files.archive {
                    ui.label(format!("Read-only ZIP: {} members · no automatic extraction",archive.entries.len()));
                    ui.text_edit_singleline(&mut self.data_files.filter);
                    egui::ScrollArea::vertical().max_height(240.0).show(ui,|ui| {
                        for(index,entry)in archive.entries.iter().enumerate().filter(|(_,entry)|entry.name.contains(&self.data_files.filter)) {
                            if ui.selectable_label(false,format!("{} · {} bytes{}",entry.name,entry.size,if entry.safe{""}else{" · unsafe name"})).clicked(){pick=Some(index);}
                        }
                    });
                } else {
                    ui.horizontal_wrapped(|ui| {
                        for(label,value)in [("Format",Command::Format),("Validate",Command::Validate),("Copy JSON path",Command::JsonPath),("Show as table",Command::Table),("Schema mappings…",Command::Schema),("Open preview",Command::Preview)] {
                            if ui.button(crate::i18n::tr_static(label)).clicked(){command=Some(value);}
                        }
                    });
                    ui.horizontal_wrapped(|ui| {
                        for(index,label)in ["Structure","Content / preview","XML / Shell / VCS tools","Plugin formats"].into_iter().enumerate(){ui.selectable_value(&mut self.data_files.tab,index,label);}
                    });
                    match self.data_files.tab {
                        0=> {
                            let rows=&self.data_files.outline;
                            egui::ScrollArea::vertical().max_height(450.0).show(ui,|ui| {
                                for row in rows {
                                    if ui.selectable_label(false,format!("{}{}","  ".repeat(row.depth),row.name)).clicked(){jump=Some(row.range.start());}
                                }
                            });
                        }
                        1=>{if self.data_files.preview.is_empty(){ui.add(egui::TextEdit::multiline(&mut self.data_files.source).code_editor().interactive(false).desired_width(f32::INFINITY));}}
                        2=> {
                            ui.label("XSD path / schema");ui.text_edit_singleline(&mut self.data_files.schema);
                            ui.horizontal_wrapped(|ui| {
                                for(label,value)in [("Validate XSD",Command::XmlSchema),("Validate DTD",Command::Dtd),("Generate XML from XSD",Command::GenerateXml),("ShellCheck",Command::ShellCheck),("shfmt",Command::Shfmt),("Format TOML",Command::Taplo),("Prettier",Command::Prettier),("Apply patch…",Command::Patch),("Run file",Command::Run)] {
                                    if ui.button(crate::i18n::tr_static(label)).clicked(){command=Some(value);}
                                }
                            });
                            ui.horizontal(|ui|{ui.label("XPath");ui.text_edit_singleline(&mut self.data_files.xpath);if ui.button("Evaluate XPath").clicked(){command=Some(Command::XPath);}});
                            if self.data_files.path.extension().is_some_and(|extension|extension=="patch"||extension=="diff") {crate::git_panel::diff_text_ui(ui,&self.theme,&self.data_files.source);}
                        }
                        3=> {
                            ui.label("Optional providers are installed and managed by the existing plugin marketplace.");
                            for(label,key)in [("Ansible / Helm","Ansible Helm"),("Makefile","Makefile"),("Log / Ideolog","Ideolog"),("PDF viewer","PDF"),("Binary / hex viewer","Hex editor"),("Advanced JS/TS language service","TypeScript")]{if ui.button(crate::i18n::tr_static(label)).clicked(){command=Some(Command::Plugin(key));}}
                            ui.label("Properties use plain text. Images, SVG and notebooks retain their existing native viewer routes.");
                        }
                        _=>{}
                    }
                }
                if !self.data_files.preview.is_empty(){egui::ScrollArea::both().max_height(420.0).show(ui,|ui|{ui.add(egui::TextEdit::multiline(&mut self.data_files.preview).code_editor().interactive(false).desired_width(f32::INFINITY));});}
                ui.label(&self.data_files.message);
                if self.data_files.busy(){crate::a11y::spinner(ui);if ui.button("Cancel task").clicked(){self.data_files.cancel();}}
            });
        if let Some(offset) = jump {
            if let Some((view, document, version)) = self.data_files.owner {
                if let Some(index) = self.tabs.iter().position(|tab| {
                    tab.view.id == view
                        && tab.state.doc.identity() == document
                        && tab.state.doc.version() == version
                }) {
                    let position = self.tabs[index].state.doc.offset_to_position(offset);
                    self.tabs[index].state.move_primary_to(position);
                }
            }
        }
        if let Some(index) = pick {
            if let Some(archive) = &self.data_files.archive {
                self.data_files.preview=match archive.member(index){Ok(bytes)=>match String::from_utf8(bytes){Ok(text)=>text,Err(error)=>format!("Binary member ({} bytes). Install a hex/image provider or save/extract with your archive tool.",error.as_bytes().len())},Err(error)=>error};
            }
        }
        if let Some(command) = command {
            self.data_file_command(command, ctx);
        }
        if !open {
            self.data_files.open = false;
            self.dialog = Dialog::None;
        }
    }
    fn data_file_command(&mut self, command: Command, ctx: &egui::Context) {
        if matches!(
            command,
            Command::XmlSchema
                | Command::Dtd
                | Command::XPath
                | Command::ShellCheck
                | Command::Shfmt
                | Command::Taplo
                | Command::Prettier
        ) {
            if !self.data_owner_current() {
                self.data_files.message =
                    "Document changed; reopen its data tools before running an external tool"
                        .into();
                return;
            }
            if matches!(command, Command::Shfmt | Command::Taplo | Command::Prettier) {
                let Some(index) = self.active else { return };
                if !self.data_source_writable(index) {
                    return;
                }
            }
        }
        let project_safe_mode = self.project_safe_mode();
        let path = self.data_files.path.clone();
        let source = self.data_files.source.clone();
        match command {
            Command::InstallRequirement => {
                if let Some((_, spec)) = self.data_files.requirement.clone() {
                    self.install_package(&spec);
                }
            }
            Command::UpgradeRequirement => {
                if !project_safe_mode {
                    if let Some((name, _)) = self.data_files.requirement.clone() {
                        self.configure_package_context();
                        if let Some(error) = self.packages_panel.upgrade_from_editor(&name) {
                            self.set_status(error);
                        } else {
                            self.bottom = Some(BottomTool::Packages);
                        }
                    }
                }
            }
            Command::Format => self.data_format_active(),
            Command::Validate => {
                if !matches!(
                    path.extension().and_then(|extension| extension.to_str()),
                    Some(
                        "json"
                            | "json5"
                            | "jsonc"
                            | "jsonl"
                            | "ndjson"
                            | "yaml"
                            | "yml"
                            | "toml"
                            | "xml"
                            | "xsd"
                    )
                ) {
                    self.data_files.message="Use the editor inspection diagnostics; external validators are available under XML / Shell / VCS tools".into();
                    return;
                }
                self.data_files.message = match data_formats::value(&source, &path) {
                    Ok(parsed) => {
                        if parsed.nonfinite {
                            self.data_files.message = "Valid JSON5 non-finite value; JSON Schema has no Infinity/NaN number representation".into();
                            return;
                        }
                        let mappings = self
                            .settings
                            .data_files
                            .schemas
                            .iter()
                            .filter(|mapping| mapping.enabled)
                            .map(|mapping| (mapping.pattern.clone(), mapping.schema.clone()))
                            .collect::<Vec<_>>();
                        if let Some(resource) = rustpy_project::data_schemas::resource(
                            &source,
                            &path,
                            &mappings,
                            self.project.as_ref().map(|project| project.root.as_path()),
                        ) {
                            rustpy_project::schema_store::request_resource(
                                &resource,
                                !project_safe_mode,
                            );
                            let (_, snapshot) = rustpy_project::schema_store::snapshot();
                            if let Some(schema) =
                                rustpy_project::data_schemas::schema(&resource, &snapshot)
                            {
                                let issues=rustpy_analysis::inspections::framework_tools::documents::inspect_json_schema(&parsed.value.to_string(),&schema);
                                if issues.is_empty() {
                                    format!("Valid against {resource}")
                                } else {
                                    issues
                                        .into_iter()
                                        .map(|issue| issue.diagnostic.message)
                                        .collect::<Vec<_>>()
                                        .join("\n")
                                }
                            } else {
                                format!(
                                    "Schema requested: {resource}. Validate again after it loads."
                                )
                            }
                        } else {
                            "Valid structured document; no schema mapping".into()
                        }
                    }
                    Err(error) => {
                        if path
                            .extension()
                            .is_some_and(|extension| extension == "xml" || extension == "xsd")
                        {
                            match roxmltree::Document::parse(&source) {
                                Ok(_) => {
                                    "Well-formed XML; XSD/DTD checks are separate explicit tools"
                                        .into()
                                }
                                Err(error) => error.to_string(),
                            }
                        } else {
                            format!("Byte {}: {}", error.offset, error.message)
                        }
                    }
                };
            }
            Command::JsonPath => {
                if let Some((view, document, version)) = self.data_files.owner {
                    if let Some(tab) = self.tabs.iter().find(|tab| {
                        tab.view.id == view
                            && tab.state.doc.identity() == document
                            && tab.state.doc.version() == version
                    }) {
                        match data_formats::parse_json(&source, data_formats::mode(&path)) {
                            Ok(parsed) => ctx.copy_text(data_formats::json_path(
                                &parsed,
                                usize::from(tab.state.primary().head()),
                            )),
                            Err(error) => self.data_files.message = error.message,
                        }
                    }
                }
            }
            Command::Table => match data_formats::value(&source, &path) {
                Ok(parsed) => {
                    let rows = match parsed.value {
                        serde_json::Value::Array(rows) => rows,
                        value => vec![value],
                    };
                    let mut columns = std::collections::BTreeSet::new();
                    for row in &rows {
                        if let Some(object) = row.as_object() {
                            columns.extend(object.keys().take(200).cloned());
                            if columns.len() >= 200 {
                                break;
                            }
                        }
                    }
                    let columns = if columns.is_empty() {
                        vec!["value".into()]
                    } else {
                        columns.into_iter().take(200).collect::<Vec<_>>()
                    };
                    let rows = rows
                        .iter()
                        .take(10000 / columns.len().max(1))
                        .map(|row| {
                            columns
                                .iter()
                                .map(|column| {
                                    if columns.len() == 1 && column == "value" && !row.is_object() {
                                        row.to_string()
                                    } else {
                                        row.get(column)
                                            .map(|value| {
                                                value
                                                    .as_str()
                                                    .map(str::to_owned)
                                                    .unwrap_or_else(|| value.to_string())
                                            })
                                            .unwrap_or_default()
                                    }
                                })
                                .collect()
                        })
                        .collect::<Vec<_>>();
                    self.dialog = Dialog::DataView {
                        title: path.display().to_string(),
                        total: rows.len(),
                        columns,
                        rows,
                        offset: 0,
                        filter: String::new(),
                    };
                }
                Err(error) => self.data_files.message = error.message,
            },
            Command::Schema => {
                self.settings_ui.page = settings_ui::SettingsPage::DataFiles;
                self.dialog = Dialog::Settings;
            }
            Command::Preview => {
                self.dialog = Dialog::None;
                self.open_web_file(&path, None, true);
            }
            Command::XmlSchema => {
                let schema = self.data_files.schema.clone();
                self.data_external(
                    "Validate XML schema",
                    self.settings.data_files.xmllint.clone(),
                    vec![
                        "--nonet".into(),
                        "--noout".into(),
                        "--schema".into(),
                        schema,
                        "$FilePath$".into(),
                    ],
                    path,
                );
            }
            Command::Dtd => self.data_external(
                "Validate XML DTD",
                self.settings.data_files.xmllint.clone(),
                vec![
                    "--nonet".into(),
                    "--noout".into(),
                    "--valid".into(),
                    "$FilePath$".into(),
                ],
                path,
            ),
            Command::XPath => {
                let expression = self.data_files.xpath.clone();
                self.data_external(
                    "Evaluate XPath",
                    self.settings.data_files.xmllint.clone(),
                    vec![
                        "--nonet".into(),
                        "--xpath".into(),
                        expression,
                        "$FilePath$".into(),
                    ],
                    path,
                );
            }
            Command::GenerateXml => {
                match read_schema(&self.data_files.schema, &path)
                    .and_then(|schema| crate::editor::data_documents::xml_from_schema(&schema))
                {
                    Ok(xml) => {
                        let mut state = EditorState::from_text(&xml, Language::Html);
                        state.path = None;
                        self.push_tab(state);
                        self.data_files.preview = xml;
                    }
                    Err(error) => self.data_files.message = error,
                }
            }
            Command::ShellCheck => self.data_external(
                "ShellCheck",
                self.settings.data_files.shellcheck.clone(),
                vec!["--format=gcc".into(), "$FilePath$".into()],
                path,
            ),
            Command::Shfmt => self.data_external(
                "shfmt",
                self.settings.data_files.shfmt.clone(),
                vec!["-w".into(), "$FilePath$".into()],
                path,
            ),
            Command::Taplo => self.data_external(
                "Format TOML",
                self.settings.data_files.toml_formatter.clone(),
                vec!["fmt".into(), "$FilePath$".into()],
                path,
            ),
            Command::Prettier => {
                let executable = self.settings.save_actions.prettier.executable.clone();
                self.data_external(
                    "Prettier",
                    if executable.trim().is_empty() {
                        "prettier".into()
                    } else {
                        executable
                    },
                    vec!["--write".into(), "$FilePath$".into()],
                    path,
                );
            }
            Command::Patch => self.data_apply_patch(),
            Command::Run => {
                if let Some(index) = self
                    .tabs
                    .iter()
                    .position(|tab| tab.state.path.as_ref() == Some(&path))
                {
                    self.data_run_file(index);
                }
            }
            Command::Plugin(query) => self.data_find_plugin(query.into()),
        }
    }
    fn data_external(&mut self, name: &str, program: String, args: Vec<String>, path: PathBuf) {
        let tool = rustpy_settings::ExternalTool {
            name: name.into(),
            group: "Data file tools".into(),
            program,
            arguments: crate::run_anything::quote_args(&args),
            working_dir: "$FileDir$".into(),
            show_console: true,
            shortcut: String::new(),
            options: rustpy_settings::external_tools::Options {
                synchronize: true,
                ..Default::default()
            },
        };
        self.run_tool(tool, Some(path), None);
    }
    pub(super) fn data_format_active(&mut self) {
        if matches!(self.dialog, Dialog::DataFiles) && !self.data_owner_current() {
            self.data_files.message =
                "Document changed; reopen its data tools before formatting".into();
            return;
        }
        let index = if matches!(self.dialog, Dialog::DataFiles) {
            self.data_files.owner.and_then(|(view, document, version)| {
                self.tabs.iter().position(|tab| {
                    tab.view.id == view
                        && tab.state.doc.identity() == document
                        && tab.state.doc.version() == version
                })
            })
        } else {
            self.active
        };
        let Some(index) = index else { return };
        if !self.data_source_writable(index) {
            return;
        }
        let tab = &self.tabs[index];
        let path = tab
            .state
            .path
            .clone()
            .unwrap_or_else(|| PathBuf::from("untitled.json"));
        let source = tab.state.doc.text();
        let language = tab.state.language;
        let result = match language {
            Language::Json => data_formats::format_json(&source, data_formats::mode(&path))
                .map_err(|error| error.message),
            Language::Yaml => rustpy_format::fragments::format(
                rustpy_format::fragments::Language::Yaml,
                &source,
                Default::default(),
            )
            .map_err(|error| error.to_string()),
            Language::Html | Language::Template => rustpy_format::fragments::format(
                rustpy_format::fragments::Language::Html,
                &source,
                Default::default(),
            )
            .map_err(|error| error.to_string()),
            Language::Sql => rustpy_format::fragments::format(
                rustpy_format::fragments::Language::Sql,
                &source,
                Default::default(),
            )
            .map_err(|error| error.to_string()),
            Language::Csv => {
                let delimiter = super::csv_table::delimiter(&source);
                super::csv_table::parse_delimited(&source, delimiter)
                    .map(|table| {
                        super::csv_table::serialize(
                            &table,
                            delimiter,
                            if source.contains("\r\n") {
                                "\r\n"
                            } else {
                                "\n"
                            },
                        )
                    })
                    .map_err(|error| error.message.into())
            }
            Language::Toml => {
                self.data_external(
                    "Format TOML",
                    self.settings.data_files.toml_formatter.clone(),
                    vec!["fmt".into(), "$FilePath$".into()],
                    path,
                );
                return;
            }
            Language::Shell => {
                self.data_external(
                    "shfmt",
                    self.settings.data_files.shfmt.clone(),
                    vec!["-w".into(), "$FilePath$".into()],
                    path,
                );
                return;
            }
            Language::Css | Language::JavaScript => {
                self.data_external(
                    "Prettier",
                    "prettier".into(),
                    vec!["--write".into(), "$FilePath$".into()],
                    path,
                );
                return;
            }
            _ => {
                Err("No formatter for this data type; configure an external tool or plugin".into())
            }
        };
        match result {
            Ok(output) => {
                let len = self.tabs[index].state.doc.len();
                self.tabs[index].state.apply_text_edits(
                    vec![(TextRange::new(TextSize::new(0), len), output)],
                    "Format data file",
                );
                self.data_files.source = self.tabs[index].state.doc.text();
                self.data_files.outline =
                    crate::editor::data_documents::outline(&self.data_files.source, &path);
                self.data_files.owner = Some((
                    self.tabs[index].view.id,
                    self.tabs[index].state.doc.identity(),
                    self.tabs[index].state.doc.version(),
                ));
                self.data_files.message =
                    "Formatted through the shared document undo transaction".into();
            }
            Err(error) => self.set_status(error),
        }
    }
    fn data_apply_patch(&mut self) {
        let project_safe_mode = self.project_safe_mode();
        if !self.data_owner_current() {
            self.data_files.message = "Document changed; reopen the patch before applying".into();
            return;
        }
        if project_safe_mode || !self.save_all() || self.data_files.patch_job.is_some() {
            return;
        }
        let Some(root) = self
            .project
            .as_ref()
            .and_then(|project| project.git.as_ref())
            .map(|repo| repo.root().to_path_buf())
        else {
            self.data_files.message = "Select a Git repository before applying a patch".into();
            return;
        };
        let patch = self.data_files.source.clone();
        self.data_files.patch_job = Some(
            TaskManager::global()
                .task("Applying reviewed patch")
                .spawn(move |progress| {
                    progress.check()?;
                    let repo = rustpy_vcs::GitRepo::discover(&root).ok_or_else(|| {
                        rustpy_core::TaskError::Failed("Repository is unavailable".into())
                    })?;
                    let reservation =
                        rustpy_vcs::RepositoryReservation::try_acquire(&root, "Apply patch")
                            .map_err(|error| rustpy_core::TaskError::Failed(error.to_string()))?;
                    reservation.run(|| {
                        repo.workspace_apply_patch(&patch, false, true)
                            .map_err(|error| rustpy_core::TaskError::Failed(error.to_string()))?;
                        progress.check()?;
                        crate::git_panel::snapshot_working_tree(
                            &repo,
                            &root,
                            "Before applying patch",
                            true,
                        );
                        repo.workspace_apply_patch(&patch, false, false)
                            .map_err(|error| rustpy_core::TaskError::Failed(error.to_string()))?;
                        Ok(root)
                    })
                }),
        );
    }
    pub(super) fn data_run_file(&mut self, index: usize) {
        let project_safe_mode = self.project_safe_mode();
        if project_safe_mode {
            return;
        }
        let Some(path) = self.tabs[index].state.path.clone() else {
            return;
        };
        let cfg = match self.tabs[index].state.language {
            Language::Shell => RunConfig {
                name: format!("Shell · {}", path.display()),
                kind: RunConfigKind::ShellScript {
                    script: path.clone(),
                },
                working_dir: path.parent().map(Path::to_path_buf),
                ..Default::default()
            },
            Language::Dockerfile => RunConfig {
                name: format!("Build · {}", path.display()),
                kind: RunConfigKind::Program {
                    program: "docker".into(),
                    args: vec![
                        "build".into(),
                        "-f".into(),
                        path.to_string_lossy().into_owned(),
                        ".".into(),
                    ],
                },
                working_dir: path.parent().map(Path::to_path_buf),
                ..Default::default()
            },
            _ => {
                self.set_status("Run file is provided here for shell scripts and Dockerfiles");
                return;
            }
        };
        self.launch_config(cfg, crate::run_context::RunMode::Run);
    }
    pub(super) fn data_file_settings(&mut self, ui: &mut egui::Ui) {
        ui.heading("Data file editors and schema mappings");
        let settings = &mut self.settings.data_files;
        let mut remove = None;
        for (index, mapping) in settings.schemas.iter_mut().enumerate() {
            ui.horizontal(|ui| {
                ui.checkbox(&mut mapping.enabled, "");
                ui.text_edit_singleline(&mut mapping.pattern);
                ui.text_edit_singleline(&mut mapping.schema);
                if ui.small_button("Remove").clicked() {
                    remove = Some(index);
                }
            });
        }
        if let Some(index) = remove {
            settings.schemas.remove(index);
        }
        if ui.button("Add mapping").clicked() {
            settings
                .schemas
                .push(rustpy_settings::data_files::SchemaMapping {
                    pattern: "*.json".into(),
                    schema: String::new(),
                    enabled: true,
                });
        }
        ui.label("Schema: SchemaStore HTTPS URL or local JSON schema / XSD path. XML completion uses mapped XSD declarations; DTD/XPath validation uses xmllint. Relative paths resolve against the project. Local references stay within the selected schema directory; schema validation never performs arbitrary network retrieval.");
        for (label, value) in [
            ("ShellCheck executable", &mut settings.shellcheck),
            ("shfmt executable", &mut settings.shfmt),
            ("XML xmllint executable", &mut settings.xmllint),
            ("TOML formatter (taplo)", &mut settings.toml_formatter),
        ] {
            ui.horizontal(|ui| {
                ui.label(crate::i18n::tr_static(label));
                ui.text_edit_singleline(value);
            });
        }
        ui.checkbox(&mut settings.csv_header, "CSV/TSV header row by default");
        ui.add(
            egui::DragValue::new(&mut settings.archive_limit_mib)
                .range(1..=256)
                .suffix(" MiB ZIP browsing limit"),
        );
    }
}

impl IdeApp {
    pub(super) fn data_requirement_gutter(&mut self, index: usize, line: usize) {
        let Some(requirement) =
            rustpy_packages::parse_requirements(&self.tabs[index].state.doc.text())
                .into_iter()
                .find(|requirement| requirement.line == line + 1)
        else {
            return;
        };
        self.active = Some(index);
        self.open_data_files();
        self.data_files.requirement = Some((requirement.name, requirement.raw));
    }
    pub(super) fn data_copy_json_path(&mut self, ctx: &egui::Context) {
        let Some(index) = self.active else { return };
        let tab = &self.tabs[index];
        let Some(path) = &tab.state.path else { return };
        match data_formats::parse_json(&tab.state.doc.text(), data_formats::mode(path)) {
            Ok(parsed) => ctx.copy_text(data_formats::json_path(
                &parsed,
                usize::from(tab.state.primary().head()),
            )),
            Err(error) => self.set_status(error.message),
        }
    }
}

impl IdeApp {
    fn data_source_writable(&mut self, index: usize) -> bool {
        if self.tabs[index].state.doc.is_read_only()
            || !self.refresh_file_permissions(index)
            || self.tabs[index].state.doc.is_read_only()
        {
            self.data_files.message =
                "Read-only document; data tools cannot modify its source".into();
            return false;
        }
        true
    }
    fn data_owner_current(&self) -> bool {
        self.data_files
            .owner
            .is_some_and(|(view, document, version)| {
                self.active
                    .and_then(|index| self.tabs.get(index))
                    .is_some_and(|tab| {
                        tab.view.id == view
                            && tab.state.doc.identity() == document
                            && tab.state.doc.version() == version
                            && tab
                                .state
                                .path
                                .as_deref()
                                .unwrap_or(Path::new("untitled.json"))
                                == self.data_files.path.as_path()
                    })
            })
    }
}

fn read_schema(value: &str, document: &Path) -> Result<String, String> {
    use std::io::Read;
    let path = Path::new(value);
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        document.parent().unwrap_or(Path::new(".")).join(path)
    };
    let file = std::fs::File::open(path).map_err(|error| error.to_string())?;
    let mut source = String::new();
    file.take(2 * 1024 * 1024 + 1)
        .read_to_string(&mut source)
        .map_err(|error| error.to_string())?;
    if source.len() > 2 * 1024 * 1024 {
        return Err("XSD generation is limited to 2 MiB".into());
    }
    Ok(source)
}

//! Framework workspace: canonical project facet, source structure and existing launch authorities.
use super::*;
use crate::a11y::AccessibleCombo;
use crate::a11y::DialogWindow;
use rustpy_project::frameworks::{self, Facet};

fn legacy_framework_target(root: &Path, key: usize) -> &'static str {
    match key {
        // Keep an existing pserve configuration authoritative; RustPy's minimal
        // Pyramid scaffold instead exposes a directly runnable WSGI script.
        8 if !root.join("development.ini").is_file() && root.join("app.py").is_file() => "app.py",
        8 => "development.ini",
        // The generated wrapper captures the external web2py installation and
        // keeps applications inside the project rather than the framework tree.
        9 if root.join("run_web2py.py").is_file() => "run_web2py.py",
        _ => "web2py.py",
    }
}
#[derive(Default)]
pub(super) struct State {
    pub(super) facet: Facet,
    templates: String,
    static_dirs: String,
    filter: String,
    task: String,
    structure: Vec<(String, PathBuf, usize)>,
    custom_commands: Vec<(String, Vec<String>)>,
    message: String,
    kind: usize,
    database_names: Vec<String>,
    database_alias: String,
    create_name: String,
}
impl IdeApp {
    pub(super) fn open_frameworks(&mut self) {
        let Some(project) = &self.project else { return };
        let model = project.tree.model();
        self.frameworks = State {
            facet: model.frameworks,
            templates: model
                .template_dirs
                .iter()
                .map(|path| {
                    path.strip_prefix(&project.root)
                        .unwrap_or(path)
                        .display()
                        .to_string()
                })
                .collect::<Vec<_>>()
                .join("\n"),
            ..Default::default()
        };
        self.frameworks.database_names =
            frameworks::django_databases(&project.root, &self.frameworks.facet)
                .into_iter()
                .map(|(alias, _)| alias)
                .collect();
        self.frameworks.database_alias = self
            .frameworks
            .database_names
            .first()
            .cloned()
            .unwrap_or_default();
        self.frameworks.static_dirs = self
            .frameworks
            .facet
            .static_directories
            .iter()
            .map(|path| path.display().to_string())
            .collect::<Vec<_>>()
            .join("\n");
        self.refresh_framework_structure();
        self.dialog = Dialog::Frameworks;
    }
    fn refresh_framework_structure(&mut self) {
        self.sync_analysis_now();
        let paths = self.project_files();
        let Some(project) = &self.project else { return };
        let mut rows = Vec::new();
        let mut commands = Vec::new();
        for path in paths
            .into_iter()
            .filter(|path| path.extension().is_some_and(|ext| ext == "py"))
            .take(1024)
        {
            let Some(file) = project.analysis.file_for_path(&path) else {
                continue;
            };
            if file.text.len() > 1024 * 1024 {
                continue;
            }
            if path
                .parent()
                .and_then(|path| path.file_name())
                .is_some_and(|name| name == "commands")
                && path
                    .parent()
                    .and_then(Path::parent)
                    .and_then(Path::file_name)
                    .is_some_and(|name| name == "management")
            {
                use rustpy_syntax::ast::{self, AstNode};
                let options = file
                    .parse
                    .syntax_node()
                    .descendants()
                    .filter_map(ast::CallExpr::cast)
                    .filter(|call| {
                        call.callee().is_some_and(|callee| {
                            callee
                                .syntax()
                                .text()
                                .to_string()
                                .ends_with(".add_argument")
                        })
                    })
                    .filter_map(|call| call.arg_list())
                    .flat_map(|args| {
                        args.args()
                            .filter_map(|arg| match arg {
                                ast::AnyArg::Positional(arg) => arg.expr(),
                                _ => None,
                            })
                            .filter_map(|expr| {
                                if let ast::Expr::String(value) = expr {
                                    Some(value.value())
                                } else {
                                    None
                                }
                            })
                            .filter(|value| value.starts_with('-'))
                            .take(32)
                            .collect::<Vec<_>>()
                    })
                    .take(128)
                    .collect::<Vec<_>>();
                if let Some(name) = path
                    .file_stem()
                    .and_then(|name| name.to_str())
                    .filter(|name| !name.starts_with('_'))
                {
                    commands.push((name.to_owned(), options));
                }
            }
            let role = match path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("")
            {
                "apps.py" => "App",
                "models.py" => "Model",
                "views.py" => "View",
                "urls.py" => "URL",
                "admin.py" => "Admin",
                "forms.py" => "Form",
                _ => "",
            };
            if !role.is_empty() {
                rows.push((
                    format!(
                        "{role} · {}",
                        path.strip_prefix(&project.root).unwrap_or(&path).display()
                    ),
                    path.clone(),
                    0,
                ));
            }
            for model in rustpy_semantic::frameworks::models(&file) {
                let line = file.text[..usize::from(model.class.range.start()).min(file.text.len())]
                    .bytes()
                    .filter(|byte| *byte == b'\n')
                    .count();
                rows.push((
                    format!("{:?} · {}", model.family, model.class.name),
                    path.clone(),
                    line,
                ));
            }
        }
        self.frameworks.structure = rows;
        self.frameworks.custom_commands = commands;
    }
    pub(super) fn save_framework_facet(&mut self) -> bool {
        let Some(project) = &self.project else {
            return false;
        };
        let mut model = project.tree.model();
        model.frameworks = self.frameworks.facet.clone();
        model.frameworks.static_directories = self
            .frameworks
            .static_dirs
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(PathBuf::from)
            .collect();
        model.template_dirs = self
            .frameworks
            .templates
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(|line| {
                let path = PathBuf::from(line);
                if path.is_absolute() {
                    path
                } else {
                    project.root.join(path)
                }
            })
            .collect();
        let root = project.root.clone();
        let result = self.apply_project_model(model);
        if result {
            rustpy_project::framework_catalog::invalidate(&root, None);
        }
        result
    }
    pub(super) fn frameworks_dialog(&mut self, ctx: &egui::Context) {
        self.dialog = Dialog::Frameworks;
        let mut open = true;
        let mut task = None;
        let mut run = None;
        let mut save = false;
        let mut refresh = false;
        let mut jump = None;
        let mut related = false;
        let mut create = false;
        let mut tool = None;
        let mut shell = None;
        let mut database = false;
        egui::Window::new("Frameworks · Django Structure").id(egui::Id::new("frameworks_workspace")).open(&mut open).default_width(850.0).resizable(true).show_modal(ctx,|ui|{
 ui.horizontal_wrapped(|ui|{for(index,label)in["Django","Flask","FastAPI","Legacy / tools"].into_iter().enumerate(){ui.selectable_value(&mut self.frameworks.kind,index,label);}});
 ui.collapsing("Project support and template data languages",|ui|{let facet=&mut self.frameworks.facet;ui.checkbox(&mut facet.django,"Enable Django support");ui.checkbox(&mut facet.auto_detect,"Detect Django from manage.py");ui.horizontal(|ui|{ui.label("Settings module (empty = detected)");ui.text_edit_singleline(&mut facet.settings_module);});let mut manage=facet.manage_script.display().to_string();ui.horizontal(|ui|{ui.label("manage.py");if ui.text_edit_singleline(&mut manage).changed(){facet.manage_script=manage.into();}});ui.checkbox(&mut facet.track_models,"Track models.py changes and offer migrations");ui.checkbox(&mut facet.use_django_test_runner,"Use Django test runner (disable to use selected standard runner)");egui::ComboBox::from_id_salt("framework_template_engine").selected_text(&facet.template_engine).show_ui(ui,|ui|{for value in["Auto","Django","Jinja2","Mako","Chameleon"]{ui.selectable_value(&mut facet.template_engine,value.into(),crate::i18n::tr_static(value));}}).combo_named(crate::i18n::tr_static("Template engine"));egui::ComboBox::from_id_salt("framework_template_data").selected_text(&facet.template_data_language).show_ui(ui,|ui|{for value in["HTML","XML","Plain text","CSS","JavaScript"]{ui.selectable_value(&mut facet.template_data_language,value.into(),crate::i18n::tr_static(value));}}).combo_named(crate::i18n::tr_static("Template data language"));ui.label("Template folders (one per line, project relative)");ui.text_edit_multiline(&mut self.frameworks.templates);ui.label("Static folders (one per line)");ui.text_edit_multiline(&mut self.frameworks.static_dirs);if ui.button("Apply project support").clicked(){save=true;}});
 ui.horizontal(|ui|{ui.label("Additional options");ui.text_edit_singleline(&mut self.frameworks.facet.additional_options);});
 ui.horizontal(|ui|{ui.label("Host");ui.text_edit_singleline(&mut self.frameworks.facet.host);ui.label("Port");ui.add(egui::DragValue::new(&mut self.frameworks.facet.port).range(1..=65535));ui.checkbox(&mut self.frameworks.facet.reload,"Reload");});
 match self.frameworks.kind{0=>{ui.checkbox(&mut self.frameworks.facet.runserver_plus,"Use django-extensions runserver_plus");ui.checkbox(&mut self.frameworks.facet.django_template_debug,"Django template debugging");ui.horizontal(|ui|{for(label,key)in[("Run server",0),("Debug server",1),("Django tests",2)]{if ui.button(crate::i18n::tr_static(label)).clicked(){run=Some(key);}}
 if ui.button("Django console").clicked(){shell=Some(true);}});ui.horizontal(|ui|{ui.label("manage.py task");ui.text_edit_singleline(&mut self.frameworks.task);if ui.button("Run task").clicked(){task=Some(self.frameworks.task.clone());}});let command=self.frameworks.task.split_whitespace().next().unwrap_or("");let words=if command.is_empty(){frameworks::COMMANDS}else{frameworks::options(command)};ui.horizontal_wrapped(|ui|{for word in words{if ui.small_button(*word).clicked(){if self.frameworks.task.is_empty(){self.frameworks.task=(*word).into();}else{self.frameworks.task.push(' ');self.frameworks.task.push_str(word);}}}});let selected=self.frameworks.task.split_whitespace().next().unwrap_or("").to_owned();let custom=self.frameworks.custom_commands.iter().flat_map(|(name,options)|if selected.is_empty(){vec![name.clone()]}else if name==&selected{options.clone()}else{Vec::new()}).collect::<Vec<_>>();ui.horizontal_wrapped(|ui|{for value in custom{if ui.small_button(&value).clicked(){if self.frameworks.task.is_empty(){self.frameworks.task=value;}else{self.frameworks.task.push(' ');self.frameworks.task.push_str(&value);}}}});
 ui.horizontal(|ui|{for word in["makemigrations","migrate","showmigrations --plan","check --deploy"]{if ui.button(crate::i18n::tr_static(word)).clicked(){task=Some(word.into());}}});},1=>{ui.horizontal(|ui|{ui.label("FLASK_APP");ui.text_edit_singleline(&mut self.frameworks.facet.flask_app);});ui.horizontal(|ui|{ui.label("Environment");ui.text_edit_singleline(&mut self.frameworks.facet.flask_environment);ui.checkbox(&mut self.frameworks.facet.flask_debug,"Debug mode");ui.checkbox(&mut self.frameworks.facet.jinja_debug,"Jinja2 debugging");});ui.horizontal(|ui|{if ui.button("Run Flask").clicked(){run=Some(3);}
 if ui.button("Debug Flask").clicked(){run=Some(4);}
 if ui.button("Flask shell").clicked(){shell=Some(false);}});},2=>{ui.horizontal(|ui|{ui.label("Uvicorn app");ui.text_edit_singleline(&mut self.frameworks.facet.fastapi_app);});ui.horizontal(|ui|{if ui.button("Run FastAPI").clicked(){run=Some(5);}
 if ui.button("Debug FastAPI").clicked(){run=Some(6);}
 if ui.button("Open /docs").clicked(){tool=Some("docs");}
 if ui.button("Generate OpenAPI").clicked(){run=Some(7);}});},_=>{ui.label("Legacy adapters use installed frameworks and explicit run configurations. Deployment remains an explicit user action.");for(label,key)in[("Pyramid pserve",8),("web2py server",9),("App Engine dev server",10),("App Engine deploy (gcloud)",11),("Buildout",12),("Compile active .pyx",13),("Qt Designer (installed tool)",14),("Compile active .ui with pyuic6",15),("Validate/render Mako",16),("Validate/render Chameleon",17)]{if ui.button(crate::i18n::tr_static(label)).clicked(){run=Some(key);}}ui.label("PyQt/PySide advanced IDE integration is a community-plugin capability. Full JavaScript/TypeScript IDE features and thin-client remote development remain separate stretch items; remote SDK targets use the existing SSH/Docker/WSL interpreter providers.");}}
 ui.horizontal(|ui|{if ui.button("Endpoints / Generate HTTP request").clicked(){tool=Some("endpoints");}
 if ui.button("Go to related view/template").clicked(){related=true;}
 if ui.button("Refresh Django structure").clicked(){refresh=true;}});
 ui.horizontal(|ui|{ui.label("Create app / model / view / URL / template");ui.text_edit_singleline(&mut self.frameworks.create_name);if ui.button("Create…").clicked(){create=true;}});ui.label("Use 'app NAME', 'model Name', 'view name', 'url name' or a relative template path.");
 egui::ComboBox::from_id_salt("django_database_alias").selected_text(&self.frameworks.database_alias).show_ui(ui,|ui|{for alias in &self.frameworks.database_names{ui.selectable_value(&mut self.frameworks.database_alias,alias.clone(),alias);}}).combo_named(crate::i18n::tr_static("Database"));
 if ui.button("Import Django data source into Database…").clicked(){database=true;}
 ui.text_edit_singleline(&mut self.frameworks.filter);egui::ScrollArea::vertical().max_height(240.0).show(ui,|ui|{for(label,path,line)in &self.frameworks.structure{if label.to_lowercase().contains(&self.frameworks.filter.to_lowercase())&&ui.selectable_label(false,label).clicked(){jump=Some((path.clone(),*line));}}});ui.label(&self.frameworks.message);
 });
        if database {
            self.framework_import_database();
        }
        if save {
            self.save_framework_facet();
        }
        if refresh {
            self.refresh_framework_structure();
        }
        if let Some((path, line)) = jump {
            self.open_file(&path);
            if let Some(index) = self.active {
                self.tabs[index].state.go_to_line(line + 1, Some(1));
            }
            self.dialog = Dialog::None;
        }
        if related {
            self.dialog = Dialog::None;
            self.go_to_related_symbol();
        }
        if let Some(task) = task {
            if self.save_framework_facet() {
                self.framework_manage_task(&task);
            }
        }
        if let Some(key) = run {
            if self.save_framework_facet() {
                self.framework_launch(key);
            }
        }
        if let Some(django) = shell {
            if self.save_framework_facet() {
                self.framework_shell(django);
            }
        }
        if create {
            self.framework_create();
        }
        if let Some(tool) = tool {
            if tool == "endpoints" {
                self.dialog = Dialog::None;
                self.handle_action(Action::ToggleEndpointsView, ctx);
            } else {
                let url = format!(
                    "http://{}:{}/docs",
                    self.frameworks.facet.host, self.frameworks.facet.port
                );
                self.open_web_url(&url, None, true);
            }
        }
        if !open {
            self.dialog = Dialog::None;
        }
    }
    pub(super) fn framework_manage_task(&mut self, task: &str) {
        let Some(project) = &self.project else { return };
        let facet = project.tree.model().frameworks;
        let cfg = RunConfig {
            name: format!("Django · {task}"),
            kind: RunConfigKind::Python {
                script: facet.manage(&project.root),
            },
            parameters: task.into(),
            working_dir: Some(project.root.clone()),
            env: facet.environment(&project.root),
            temporary: true,
            ..Default::default()
        };
        self.launch_config(cfg, crate::run_context::RunMode::Run);
    }
    fn framework_launch(&mut self, key: usize) {
        use rustpy_project::configuration_types::{ConfigurationSpec, ConfigurationType as Kind};
        let Some(project) = &self.project else { return };
        let root = project.root.clone();
        let facet = project.tree.model().frameworks;
        let kinds = [
            Kind::DjangoServer,
            Kind::DjangoServer,
            Kind::DjangoTests,
            Kind::Flask,
            Kind::Flask,
            Kind::FastApi,
            Kind::FastApi,
        ];
        let mut spec =
            ConfigurationSpec::new(kinds.get(key).copied().unwrap_or(Kind::PythonModule));
        let mut cfg = RunConfig {
            name: "Framework task".into(),
            working_dir: Some(root.clone()),
            env: facet.environment(&root),
            temporary: true,
            ..Default::default()
        };
        let address = format!("{}:{}", facet.host, facet.port);
        match key {
            0 | 1 => {
                spec.target = facet.manage(&root).display().to_string();
                spec.arguments.push(address);
                if !facet.reload {
                    spec.arguments.push("--noreload".into());
                }
                if facet.runserver_plus {
                    spec.kind = Kind::DjangoTask;
                    spec.member = "runserver_plus".into();
                }
            }
            2 => {
                spec.target = facet.manage(&root).display().to_string();
            }
            3 | 4 => {
                spec.target = facet.flask_app.clone();
                spec.arguments = vec![
                    "--host".into(),
                    facet.host.clone(),
                    "--port".into(),
                    facet.port.to_string(),
                ];
                if facet.flask_debug {
                    spec.arguments.push("--debug".into());
                }
                if !facet.reload {
                    spec.arguments.push("--no-reload".into());
                }
                cfg.env
                    .insert("FLASK_ENV".into(), facet.flask_environment.clone());
            }
            5 | 6 => {
                spec.target = facet.fastapi_app.clone();
                spec.arguments = vec![
                    "--host".into(),
                    facet.host.clone(),
                    "--port".into(),
                    facet.port.to_string(),
                ];
                if facet.reload {
                    spec.arguments.push("--reload".into());
                }
            }
            7 => {
                let target = serde_json::to_string(&facet.fastapi_app).unwrap();
                let code=format!("import importlib,json; name,attr={target}.split(':',1); app=getattr(importlib.import_module(name),attr); print(json.dumps(app.openapi(),indent=2))");
                cfg.kind = RunConfigKind::Program {
                    program: self
                        .project
                        .as_ref()
                        .and_then(|project| project.interpreter.as_ref())
                        .map(|sdk| sdk.path.clone())
                        .unwrap_or_else(|| PathBuf::from("python")),
                    args: vec!["-c".into(), code],
                };
            }
            8 => {
                spec.kind = Kind::Pyramid;
                spec.target = legacy_framework_target(&root, key).into();
            }
            9 => {
                spec.kind = Kind::Web2py;
                spec.target = legacy_framework_target(&root, key).into();
            }
            10 => {
                spec.kind = Kind::AppEngine;
                spec.target = "app.yaml".into();
            }
            11 => {
                cfg.kind = RunConfigKind::Program {
                    program: "gcloud".into(),
                    args: vec!["app".into(), "deploy".into(), "app.yaml".into()],
                };
            }
            12 => {
                spec.target = "zc.buildout".into();
                spec.arguments = vec!["-c".into(), "buildout.cfg".into()];
            }
            13 => {
                let Some(path) = self
                    .active
                    .and_then(|index| self.tabs[index].state.path.clone())
                    .filter(|path| path.extension().is_some_and(|extension| extension == "pyx"))
                else {
                    self.set_status("Open a .pyx file first");
                    return;
                };
                spec.target = "cython".into();
                spec.arguments = vec!["-3".into(), path.display().to_string()];
            }
            14 | 15 => {
                let Some(path) = self
                    .active
                    .and_then(|index| self.tabs[index].state.path.clone())
                    .filter(|path| path.extension().is_some_and(|extension| extension == "ui"))
                else {
                    self.set_status("Open a Qt .ui file first");
                    return;
                };
                cfg.kind = RunConfigKind::Program {
                    program: if key == 14 { "designer" } else { "pyuic6" }.into(),
                    args: vec![path.display().to_string()],
                };
            }
            16 | 17 => {
                let Some(path) = self
                    .active
                    .and_then(|index| self.tabs[index].state.path.clone())
                else {
                    return;
                };
                let path = serde_json::to_string(&path.display().to_string()).unwrap();
                let code = if key == 16 {
                    format!("from mako.template import Template; print(Template(filename={path}).render())")
                } else {
                    format!(
                        "from chameleon import PageTemplateFile; print(PageTemplateFile({path})())"
                    )
                };
                cfg.kind = RunConfigKind::Program {
                    program: self
                        .project
                        .as_ref()
                        .and_then(|project| project.interpreter.as_ref())
                        .map(|sdk| sdk.path.clone())
                        .unwrap_or_else(|| PathBuf::from("python")),
                    args: vec!["-c".into(), code],
                };
            }
            _ => {}
        }
        if !matches!(key, 7 | 11 | 14 | 15 | 16 | 17) {
            match spec.materialize(&cfg) {
                Ok(lowered) => {
                    cfg.kind = lowered.kind;
                    cfg.type_options = Some(spec);
                }
                Err(error) => {
                    self.set_status(error);
                    return;
                }
            }
        }
        if matches!(key, 0..=6) {
            cfg.parameters = facet.additional_options.clone();
        }
        cfg.name = match key {
            0 | 1 => "Django server",
            2 => "Django tests",
            3 | 4 => "Flask server",
            5 | 6 => "FastAPI server",
            7 => "FastAPI OpenAPI",
            8 => "Pyramid",
            9 => "web2py",
            10 => "App Engine development",
            11 => "Deploy App Engine",
            12 => "Buildout",
            13 => "Compile Cython",
            14 => "Qt Designer",
            15 => "Compile Qt UI",
            16 => "Mako render",
            _ => "Chameleon render",
        }
        .into();
        if matches!(key, 1 | 4 | 6) {
            self.settings.debugger.workspace.django = key == 1 && facet.django_template_debug;
            self.settings.debugger.workspace.jinja = key == 4 && facet.jinja_debug;
        }
        self.launch_config(
            cfg,
            if matches!(key, 1 | 4 | 6) {
                crate::run_context::RunMode::Debug
            } else {
                crate::run_context::RunMode::Run
            },
        );
    }
    pub(super) fn framework_run_policy(&self, cfg: &mut RunConfig) {
        let Some(project) = &self.project else { return };
        let facet = project.tree.model().frameworks;
        if !facet.enabled(&project.root) {
            return;
        }
        for (key, value) in facet.environment(&project.root) {
            cfg.env.entry(key).or_insert(value);
        }
        if !facet.use_django_test_runner
            && cfg.type_options.as_ref().is_some_and(|spec| {
                spec.kind == rustpy_project::configuration_types::ConfigurationType::DjangoTests
            })
        {
            cfg.kind = if self.settings.testing.default_runner == "pytest" {
                RunConfigKind::Pytest {
                    target: project.root.display().to_string(),
                    keywords: String::new(),
                }
            } else {
                RunConfigKind::Unittest {
                    target: project.root.display().to_string(),
                }
            };
            cfg.type_options = None;
            cfg.parameters.clear();
        }
    }
    pub(super) fn framework_model_saved(&mut self, path: &Path) {
        if path.file_name().is_none_or(|name| name != "models.py") {
            return;
        }
        let Some(project) = &self.project else { return };
        let facet = project.tree.model().frameworks;
        if facet.track_models && facet.enabled(&project.root) {
            self.notify(
                crate::notifications::NotificationKind::Info,
                "Django",
                "Models changed",
                "Review model changes and generate migrations before updating the database.",
                vec![(
                    "Frameworks / migrations".into(),
                    crate::notifications::NotificationAction::RunAction(Action::Frameworks),
                )],
            );
        }
    }
    pub(super) fn framework_shell(&mut self, django: bool) {
        if self.safe_mode {
            return;
        }
        let Some(project) = &self.project else { return };
        let root = project.root.clone();
        let facet = project.tree.model().frameworks;
        self.py_console.add(if django { "Django" } else { "Flask" });
        if !self.ensure_py_console() {
            return;
        }
        let code = if django {
            format!("import os,sys\nsys.path.insert(0,{})\nos.environ['DJANGO_SETTINGS_MODULE']={}\nimport django; django.setup()\nfrom django.apps import apps\nglobals().update({{model.__name__:model for model in apps.get_models()}})",serde_json::to_string(&root.display().to_string()).unwrap(),serde_json::to_string(&facet.settings(&root)).unwrap())
        } else {
            format!("from flask.cli import ScriptInfo\n_rustpy_flask_app=ScriptInfo(app_import_path={}).load_app()\n_rustpy_flask_context=_rustpy_flask_app.app_context()\n_rustpy_flask_context.push()\nglobals().update(_rustpy_flask_app.make_shell_context())",serde_json::to_string(&facet.flask_app).unwrap())
        };
        self.py_console.execute_fragment(&code);
        self.bottom = Some(BottomTool::PythonConsole);
    }
    fn framework_create(&mut self) {
        let value = self.frameworks.create_name.trim().to_owned();
        if let Some(name) = value.strip_prefix("app ") {
            if identifier(name) {
                self.framework_manage_task(&format!("startapp {name}"));
            } else {
                self.frameworks.message = "Use a Python identifier for the app name".into();
            }
            return;
        }
        let Some(project) = &self.project else { return };
        let root = project.root.clone();
        let (directory, name, source) = if let Some(name) = value.strip_prefix("model ") {
            (
                &root,
                name.to_owned(),
                format!("from django.db import models\n\nclass {name}(models.Model):\n    pass\n"),
            )
        } else if let Some(name) = value.strip_prefix("view ") {
            (&root,name.to_owned(),format!("from django.shortcuts import render\n\ndef {name}(request):\n    return render(request, '{name}.html', {{}})\n"))
        } else if let Some(name) = value.strip_prefix("url ") {
            (&root,name.to_owned(),format!("from django.urls import path\nfrom . import views\n\nurlpatterns = [path('{name}/', views.{name}, name='{name}')]\n"))
        } else {
            let directory = project
                .tree
                .template_dirs
                .iter()
                .next()
                .cloned()
                .unwrap_or_else(|| root.join("templates"));
            self.open_new_file_dialog(directory, "HTML File".into());
            return;
        };
        if !identifier(&name) {
            self.frameworks.message = "Use a Python identifier".into();
            return;
        }
        let path = directory.join(format!("{name}.py"));
        let plan = rustpy_project::project_templates::TemplatePlan {
            files: vec![rustpy_project::project_templates::TemplateFile {
                path: path.strip_prefix(&root).unwrap_or(&path).to_path_buf(),
                contents: source,
            }],
            ..Default::default()
        };
        if let Err(error) = plan.write_new(&root) {
            self.frameworks.message = error.to_string();
            return;
        }
        self.refresh_project_files();
        self.open_file(&path);
        self.dialog = Dialog::None;
    }
}

fn identifier(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(rustpy_core::is_ident_start)
        && chars.all(rustpy_core::is_ident_continue)
}

impl IdeApp {
    fn framework_import_database(&mut self) {
        let Some(project) = &self.project else { return };
        let facet = project.tree.model().frameworks;
        let entries = frameworks::django_databases(&project.root, &facet);
        let Some((alias, values)) = entries
            .into_iter()
            .find(|(alias, _)| alias == &self.frameworks.database_alias)
        else {
            self.frameworks.message="No literal Django DATABASES entry found. Enter environment-derived credentials in Database properties.".into();
            return;
        };
        match rustpy_db::injection::from_django_literals(0, &alias, &values) {
            Ok((source, secret)) => {
                self.db_panel.import_database_source(source, secret);
                self.dialog = Dialog::None;
                self.bottom = Some(BottomTool::Database);
            }
            Err(error) => self.frameworks.message = error.to_string(),
        }
    }
    pub(super) fn framework_settings_page(&mut self, ui: &mut egui::Ui) {
        ui.heading("Framework support (project)");
        if let Some(project) = &self.project {
            let facet = project.tree.model().frameworks;
            ui.label(format!(
                "Django settings: {}",
                facet.settings(&project.root)
            ));
            ui.label(format!(
                "Manage script: {}",
                facet.manage(&project.root).display()
            ));
        }
        if ui
            .button("Configure Django / Flask / FastAPI and templates…")
            .clicked()
        {
            self.open_frameworks();
            self.pending_dialog = Some(Dialog::Frameworks);
        }
    }
}

impl IdeApp {
    /// Template roots in lookup order: marked Template Folders, Django
    /// `TEMPLATES` `DIRS`, the project `templates/` and each app's `templates/`.
    pub(super) fn template_directories(&self) -> Vec<PathBuf> {
        let Some(project) = &self.project else {
            return Vec::new();
        };
        let mut configured: Vec<_> = project.tree.template_dirs.iter().cloned().collect();
        configured.sort();
        let facet = project.tree.model().frameworks;
        configured.extend(frameworks::settings_template_dirs(
            &project.root,
            &facet.settings(&project.root),
        ));
        frameworks::resource_directories(&project.root, &configured, "templates")
    }
    /// A literal template name resolved like `{% import %}`/`{% include %}`:
    /// the template roots first, then any `templates` folder of the project.
    pub(super) fn template_path(&self, name: &str) -> Option<PathBuf> {
        if !rustpy_semantic::framework_templates::safe(name) {
            return None;
        }
        self.project.as_ref()?;
        if let Some(path) = self
            .template_directories()
            .into_iter()
            .map(|directory| directory.join(name))
            .find(|path| path.is_file())
        {
            return Some(path);
        }
        let depth = Path::new(name).components().count();
        self.project_files().into_iter().find(|path| {
            path.ends_with(name)
                && path
                    .ancestors()
                    .nth(depth)
                    .and_then(Path::file_name)
                    .is_some_and(|folder| folder == "templates")
        })
    }
    /// Current text of a template: the open buffer, else the bounded file.
    pub(super) fn template_text(&self, path: &Path) -> Option<String> {
        if let Some(tab) = self
            .tabs
            .iter()
            .find(|tab| tab.state.path.as_deref() == Some(path))
        {
            return Some(tab.state.doc.text());
        }
        std::fs::metadata(path)
            .ok()
            .filter(|meta| meta.len() <= 2 * 1024 * 1024)?;
        std::fs::read_to_string(path).ok()
    }
    /// Go to the definition of the Jinja2 macro referenced at `offset`
    /// (same-file call, `alias.name()` or a `from ... import` name).
    pub(super) fn jinja_macro_navigation(&mut self, offset: TextSize) -> bool {
        let Some(index) = self.active else {
            return false;
        };
        if self.tabs[index].state.language != Language::Template {
            return false;
        }
        let text = self.tabs[index].state.doc.text();
        let Some(target) =
            rustpy_semantic::template_jinja::reference_at(&text, usize::from(offset))
        else {
            return false;
        };
        let location = match target {
            rustpy_semantic::template_jinja::MacroTarget::Local(name) => self.tabs[index]
                .state
                .path
                .clone()
                .zip(rustpy_semantic::template_jinja::definition(&text, &name)),
            rustpy_semantic::template_jinja::MacroTarget::Imported { template, name } => {
                self.template_path(&template).map(|path| {
                    let range = self
                        .template_text(&path)
                        .and_then(|source| {
                            rustpy_semantic::template_jinja::definition(&source, &name)
                        })
                        .unwrap_or(TextRange::empty(TextSize::new(0)));
                    (path, range)
                })
            }
        };
        let Some((path, range)) = location else {
            self.set_status("Cannot find macro definition");
            return true;
        };
        self.navigate_locations(
            vec![features::Location {
                path,
                range,
                is_stub: false,
            }],
            "Cannot find macro definition",
        );
        true
    }
    pub(super) fn framework_template_related(&mut self) -> bool {
        let Some(index) = self.active else {
            return false;
        };
        let head = self.tabs[index].state.primary().head();
        if self.template_url_declaration(head) {
            return true;
        }
        if self.tabs[index].state.language == Language::Template
            && self.jinja_macro_navigation(head)
        {
            return true;
        }
        self.template_link_navigation(head)
    }
    /// Go to the template named by `{% extends/include/import/from %}`, the
    /// static file of `{% static %}`, or the parent template's `{% block %}`.
    pub(super) fn template_link_navigation(&mut self, offset: TextSize) -> bool {
        let Some(index) = self.active else {
            return false;
        };
        if self.tabs[index].state.language != Language::Template
            || self.tabs[index].state.path.is_none()
        {
            return false;
        }
        let text = self.tabs[index].state.doc.text();
        let links = rustpy_semantic::framework_templates::links(&text);
        let Some(link) = links
            .iter()
            .find(|link| link.range.contains_inclusive(offset) && link.kind != "macro")
        else {
            return false;
        };
        let Some(project) = &self.project else {
            return false;
        };
        let location = match link.kind.as_str() {
            "static" => {
                let directories: Vec<_> = project
                    .tree
                    .model()
                    .frameworks
                    .static_directories
                    .iter()
                    .map(|directory| {
                        if directory.is_absolute() {
                            directory.clone()
                        } else {
                            project.root.join(directory)
                        }
                    })
                    .collect();
                let paths = self.project_files();
                rustpy_semantic::framework_templates::target(link, &paths, &directories)
                    .map(|target| (target.clone(), TextRange::empty(TextSize::new(0))))
            }
            "block" | "endblock" => links
                .iter()
                .find(|link| link.kind == "extends")
                .and_then(|parent| self.template_path(&parent.name))
                .and_then(|parent| {
                    let source = self.template_text(&parent)?;
                    let block = rustpy_semantic::framework_templates::links(&source)
                        .into_iter()
                        .find(|block| block.kind == "block" && block.name == link.name)?;
                    Some((parent, block.range))
                }),
            _ => self
                .template_path(&link.name)
                .map(|target| (target, TextRange::empty(TextSize::new(0)))),
        };
        let Some((path, range)) = location else {
            if matches!(link.kind.as_str(), "block" | "endblock") {
                return false;
            }
            self.set_status(format!("Cannot find template '{}'", link.name));
            return true;
        };
        self.navigate_locations(
            vec![features::Location {
                path,
                range,
                is_stub: false,
            }],
            "No template target",
        );
        true
    }
}

impl IdeApp {
    pub(super) fn framework_create_template(&mut self, name: &str) {
        if !rustpy_semantic::framework_templates::safe(name) {
            return;
        }
        let Some(project) = &self.project else { return };
        let root = project.root.clone();
        let directory = project
            .tree
            .template_dirs
            .iter()
            .next()
            .cloned()
            .unwrap_or_else(|| root.join("templates"));
        let Ok(relative) = directory
            .join(name)
            .strip_prefix(&root)
            .map(Path::to_path_buf)
        else {
            self.set_status("Create attached templates through File → New in their marked root");
            return;
        };
        let plan=rustpy_project::project_templates::TemplatePlan{files:vec![rustpy_project::project_templates::TemplateFile{path:relative.clone(),contents:"<!DOCTYPE html>\n<html><body>\n{% block content %}{% endblock %}\n</body></html>\n".into()}],..Default::default()};
        match plan.write_new(&root) {
            Ok(()) => {
                self.refresh_project_files();
                self.open_file(&root.join(relative));
            }
            Err(error) => self.set_status(error.to_string()),
        }
    }
}

impl IdeApp {
    pub(super) fn framework_document_completion(&mut self, explicit: bool) -> bool {
        let Some(index) = self.active else {
            return false;
        };
        let tab = &self.tabs[index];
        if tab.state.is_large_file() || usize::from(tab.state.doc.len()) > 2 * 1024 * 1024 {
            return false;
        }
        let source = tab.state.doc.text();
        let end = usize::from(tab.state.primary().head()).min(source.len());
        let path = tab.state.path.clone().unwrap_or_default();
        let configured = self
            .project
            .as_ref()
            .map(|project| project.tree.model().frameworks.template_engine)
            .unwrap_or_default();
        let words = if path.file_name().is_some_and(|name| name == "buildout.cfg") {
            vec![
                "[buildout]",
                "parts",
                "develop",
                "extends",
                "eggs-directory",
                "download-cache",
                "find-links",
                "index",
                "versions",
                "allow-picked-versions",
                "offline",
                "newest",
                "relative-paths",
                "recipe",
                "eggs",
                "interpreter",
                "extra-paths",
                "scripts",
            ]
        } else if tab.state.language == Language::Template {
            let Some(engine) = rustpy_syntax::template_alternate::engine(
                &source,
                path.extension()
                    .and_then(|ext| ext.to_str())
                    .unwrap_or_default(),
                &configured,
            ) else {
                return false;
            };
            rustpy_syntax::template_alternate::completions(engine).to_vec()
        } else {
            return false;
        };
        let before = &source[..end];
        let start = before
            .rfind(|character: char| {
                !(rustpy_core::is_ident_continue(character) || matches!(character, ':' | '-' | '['))
            })
            .map_or(0, |at| {
                at + before[at..].chars().next().map_or(0, char::len_utf8)
            });
        let prefix = &source[start..end];
        let mut items = words
            .into_iter()
            .filter(|word| word.to_lowercase().starts_with(&prefix.to_lowercase()))
            .map(|word| CompletionItem {
                label: word.into(),
                insert: word.into(),
                detail: "Framework document".into(),
                kind: CompletionKind::Keyword,
                caret_offset: None,
                priority: 0,
            })
            .collect::<Vec<_>>();
        if path.file_name().is_some_and(|name| name == "buildout.cfg")
            && before
                .rsplit('\n')
                .next()
                .is_some_and(|line| line.contains('='))
        {
            if let Some(project) = &self.project {
                if let Ok(children) = std::fs::read_dir(&project.root) {
                    for child in children.take(256).flatten() {
                        let name = child.file_name().to_string_lossy().into_owned();
                        if name.starts_with(prefix) {
                            let directory = child.file_type().is_ok_and(|kind| kind.is_dir());
                            items.push(CompletionItem {
                                label: name.clone().into(),
                                insert: if directory { format!("{name}/") } else { name },
                                detail: "Buildout project path".into(),
                                kind: CompletionKind::Variable,
                                caret_offset: None,
                                priority: 0,
                            });
                        }
                    }
                }
            }
        }
        let (template, version, head) = (
            tab.state.language == Language::Template,
            tab.state.doc.version(),
            tab.state.primary().head(),
        );
        let mut loading = false;
        if template
            && self.project.is_some()
            && before
                .rsplit('\n')
                .next()
                .is_some_and(|line| line.contains("${") || line.contains("tal:"))
        {
            // View render-context names, inferred off the UI thread.
            let name_start = before
                .rfind(|character: char| !rustpy_core::is_ident_continue(character))
                .map_or(0, |at| {
                    at + before[at..].chars().next().map_or(0, char::len_utf8)
                });
            match self.template_context_candidates(
                &path,
                name_start,
                explicit,
                super::completion_worker::TemplateQuery::Names,
            ) {
                Some(names) => {
                    for mut item in names
                        .into_iter()
                        .filter(|item| item.label.starts_with(prefix))
                    {
                        item.detail = "View render context".into();
                        items.push(item);
                    }
                }
                None => loading = true,
            }
        }
        if items.is_empty() && !loading {
            return false;
        }
        let count = items.len();
        self.completion = CompletionPopup {
            visible: true,
            items,
            imports: vec![None; count],
            selected: if count > 0 { 0 } else { usize::MAX },
            prefix_start: TextSize::new(start as u32),
            doc_version: version,
            caret: head,
            rect: None,
            explicit,
        };
        self.import_completion = None;
        self.completion_ui.paths = true;
        self.completion_ui.due = None;
        true
    }
}

#[cfg(test)]
mod configured_cython_tests {
    use super::*;
    use std::time::{Duration, Instant};

    struct CompilerFixture {
        app: IdeApp,
        root: tempfile::TempDir,
    }
    impl Drop for CompilerFixture {
        fn drop(&mut self) {
            for session in &mut self.app.run_panel.sessions {
                session.stop();
            }
        }
    }

    fn configured_compiler() -> CompilerFixture {
        let interpreter = PathBuf::from(
            std::env::var_os("RUSTPY_CYTHON_PYTHON")
                .expect("set RUSTPY_CYTHON_PYTHON to an admitted absolute SDK with Cython"),
        );
        assert!(interpreter.is_absolute() && interpreter.is_file());
        // Preserve venv executable spelling, including bin/python symlinks.
        let mut command = std::process::Command::new(&interpreter);
        command.args([
            "-I",
            "-c",
            "import Cython; print('RUSTPY_CYTHON_READY:' + Cython.__version__)",
        ]);
        let (status, out, err) = rustpy_project::native_skeleton::supervise_tool_status(
            &mut command,
            &rustpy_core::CancellationToken::new(),
            Duration::from_secs(5),
            262144,
        )
        .expect("admitted Cython provider preflight must complete");
        assert!(
            status.success() && String::from_utf8_lossy(&out).contains("RUSTPY_CYTHON_READY:"),
            "Cython must be installed in the admitted SDK: {}",
            String::from_utf8_lossy(&err)
        );
        let sdk =
            rustpy_project::interpreter::probe_detailed(&interpreter, "configured-acceptance")
                .expect("admitted SDK must pass the production interpreter probe")
                .interpreter;
        assert_eq!(
            sdk.path, interpreter,
            "selected SDK spelling must be retained"
        );
        let root = tempfile::Builder::new()
            .prefix("cython π workspace ")
            .tempdir()
            .unwrap();
        let mut settings = Settings::default();
        settings.trust_project(root.path());
        let mut app = IdeApp::new(settings, Some(root.path().into()));
        let project = app.project.as_mut().unwrap();
        project.interpreter_rx = None;
        project.interpreter = Some(sdk);
        CompilerFixture { app, root }
    }

    fn compile_active(fixture: &mut CompilerFixture, path: &Path) -> (i32, String) {
        fixture.app.open_file(path);
        let index = fixture.app.active.unwrap();
        assert_eq!(fixture.app.tabs[index].state.language, Language::Cython);
        let source = fixture.app.tabs[index].state.doc.text();
        let version = fixture.app.tabs[index].state.doc.version();
        let identity = fixture.app.tabs[index].state.doc.identity();
        fixture.app.tabs[index].state.doc.set_read_only(true);
        let session_id = fixture.app.next_session_id;
        // This is the actual Compile Cython consumer used by the Frameworks
        // dialog, including configuration lowering, SDK routing and Run output.
        fixture.app.framework_launch(13);
        let captured = fixture
            .app
            .run_panel
            .launcher
            .launched
            .get(&session_id)
            .expect("Compile Cython must retain its exact launched configuration")
            .clone();
        assert!(
            matches!(&captured.kind, RunConfigKind::PythonModule { module } if module == "cython")
        );
        assert_eq!(captured.working_dir.as_deref(), Some(fixture.root.path()));
        let spec = captured
            .type_options
            .as_ref()
            .expect("typed Cython module route");
        assert_eq!(spec.target, "cython");
        assert_eq!(
            spec.arguments,
            vec!["-3".to_string(), path.display().to_string()]
        );
        let sdk = &fixture
            .app
            .project
            .as_ref()
            .unwrap()
            .interpreter
            .as_ref()
            .unwrap()
            .path;
        let session = fixture
            .app
            .run_panel
            .sessions
            .iter_mut()
            .find(|session| session.id == session_id)
            .expect("Compile Cython must publish an actual Run session");
        // Each newly compiled source keeps the existing unique configuration
        // name; the captured session/configuration identity is authoritative.
        assert_eq!(session.name, captured.name);
        assert!(session.command_line.contains(sdk.to_str().unwrap()));
        assert!(session.command_line.contains(path.to_str().unwrap()));
        assert!(session.command_line.contains("cython"));
        assert!(session.command_line.contains("-3"));
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            session.poll();
            if !session.is_running() {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "Cython child did not finish: {}",
                session.text()
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        let output = session.text();
        let code = match &session.status {
            rustpy_run::SessionStatus::Exited(code) => *code,
            status => panic!("Cython did not exit normally: {status:?}\n{output}"),
        };
        let state = &fixture.app.tabs[index].state;
        assert_eq!(state.doc.identity(), identity);
        assert_eq!(state.doc.version(), version);
        assert_eq!(state.doc.text(), source);
        assert!(state.doc.is_read_only());
        assert_eq!(std::fs::read_to_string(path).unwrap(), source);
        (code, output)
    }

    #[test]
    #[ignore = "requires RUSTPY_CYTHON_PYTHON with real Cython; central configured acceptance"]
    fn configured_compile_cython_generates_c_and_reports_real_import_and_type_errors() {
        let mut fixture = configured_compiler();
        let dependency = fixture.root.path().join("dependency.pxd");
        let declarations = "ctypedef int Counter\ncdef inline Counter add_one(Counter value):\n    return value + 1\n";
        std::fs::write(&dependency, declarations).unwrap();
        let valid = fixture.root.path().join("unicode_source.pyx");
        let source = "# cython: language_level=3\n# Unicode path and binding: π\nfrom dependency cimport Counter, add_one\ncpdef Counter increase(Counter π):\n    return add_one(π)\n";
        std::fs::write(&valid, source).unwrap();
        let (code, output) = compile_active(&mut fixture, &valid);
        assert_eq!(code, 0, "{output}");
        let generated_path = valid.with_extension("c");
        let generated = std::fs::read_to_string(&generated_path)
            .expect("actual Cython compiler must create the generated C source");
        assert!(generated.contains("Generated by Cython"));
        assert!(generated.contains("increase") && generated.contains("add_one"));
        assert_eq!(std::fs::read_to_string(&dependency).unwrap(), declarations);
        for (name, broken, diagnostic) in [
            (
                "missing_import.pyx",
                "from rustpy_absent_cython_dependency cimport Missing\n",
                "rustpy_absent_cython_dependency",
            ),
            (
                "invalid_type.pyx",
                "cdef int* invalid_pointer = 1\n",
                "Cannot assign type",
            ),
        ] {
            let path = fixture.root.path().join(name);
            std::fs::write(&path, broken).unwrap();
            let (code, output) = compile_active(&mut fixture, &path);
            assert_ne!(code, 0, "real Cython must reject {name}: {output}");
            assert!(output.contains(diagnostic), "{name}: {output}");
            assert!(
                output.contains(name),
                "compiler diagnostics retain source identity: {output}"
            );
            // Cython versions may remove failed output or retain a deliberately
            // unusable #error artifact. Neither may replace successful output.
            if let Ok(failed_output) = std::fs::read_to_string(path.with_extension("c")) {
                assert!(
                    failed_output.contains("#error"),
                    "failed compilation published usable C"
                );
            }
        }
        assert_eq!(std::fs::read_to_string(generated_path).unwrap(), generated);
        assert_eq!(std::fs::read_to_string(&valid).unwrap(), source);
        assert_eq!(std::fs::read_to_string(dependency).unwrap(), declarations);
    }
}

#[cfg(test)]
#[path = "frameworks_ui/configured_template_tests.rs"]
mod configured_template_tests;

#[cfg(test)]
mod legacy_action_targets {
    use super::*;
    use rustpy_project::{
        configuration_types::{ConfigurationSpec, ConfigurationType},
        project_templates::{plan, TemplateOptions},
        setup::ProjectKind,
    };

    #[test]
    fn pyramid_action_runs_generated_entry_and_preserves_existing_pserve_configuration() {
        let root = tempfile::tempdir().unwrap();
        let scaffold = plan(ProjectKind::Pyramid, &TemplateOptions::default()).unwrap();
        let entry = scaffold
            .files
            .iter()
            .find(|file| file.path == Path::new("app.py"))
            .unwrap();
        std::fs::write(root.path().join(&entry.path), &entry.contents).unwrap();
        let mut spec = ConfigurationSpec::new(ConfigurationType::Pyramid);
        spec.target = legacy_framework_target(root.path(), 8).into();
        let cfg = spec.materialize(&RunConfig::default()).unwrap();
        assert!(
            matches!(cfg.kind, RunConfigKind::Python { script } if script == Path::new("app.py"))
        );
        std::fs::write(root.path().join("development.ini"), "[app:main]\n").unwrap();
        spec.target = legacy_framework_target(root.path(), 8).into();
        let cfg = spec.materialize(&RunConfig::default()).unwrap();
        assert!(
            matches!(cfg.kind, RunConfigKind::PythonModule { module } if module == "pyramid.scripts.pserve")
        );
        assert!(cfg.parameters.contains("development.ini"));
    }

    #[test]
    fn web2py_action_prefers_project_wrapper_and_retains_conventional_entry() {
        let root = tempfile::tempdir().unwrap();
        assert_eq!(legacy_framework_target(root.path(), 9), "web2py.py");
        std::fs::write(
            root.path().join("web2py.py"),
            "# conventional installation\n",
        )
        .unwrap();
        std::fs::write(
            root.path().join("run_web2py.py"),
            "# captured project wrapper\n",
        )
        .unwrap();
        let mut spec = ConfigurationSpec::new(ConfigurationType::Web2py);
        spec.target = legacy_framework_target(root.path(), 9).into();
        let cfg = spec.materialize(&RunConfig::default()).unwrap();
        assert!(
            matches!(cfg.kind, RunConfigKind::Python { script } if script == Path::new("run_web2py.py"))
        );
        std::fs::remove_file(root.path().join("run_web2py.py")).unwrap();
        assert_eq!(legacy_framework_target(root.path(), 9), "web2py.py");
    }
}

#[cfg(test)]
#[path = "frameworks_ui/configured_legacy_tests.rs"]
mod configured_legacy_tests;

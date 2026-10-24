//! Declarative new-project scaffolds. Planning never executes an interpreter,
//! framework generator or installer; every produced path is project relative.
use crate::setup::ProjectKind;
use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TemplateEngine {
    Django,
    Jinja2,
    None,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TemplateOptions {
    pub project_name: String,
    pub create_main: bool,
    pub django_app_name: String,
    pub django_admin: bool,
    pub django_template_engine: TemplateEngine,
    pub django_templates_dir: String,
    pub flask_template_engine: TemplateEngine,
    pub flask_templates_dir: String,
    pub fastapi_async: bool,
    pub web2py_framework: Option<PathBuf>,
    pub appengine_runtime: String,
}
impl Default for TemplateOptions {
    fn default() -> Self {
        Self {
            project_name: "myproject".into(),
            create_main: true,
            django_app_name: "app".into(),
            django_admin: true,
            django_template_engine: TemplateEngine::Django,
            django_templates_dir: "templates".into(),
            flask_template_engine: TemplateEngine::Jinja2,
            flask_templates_dir: "templates".into(),
            fastapi_async: true,
            web2py_framework: None,
            appengine_runtime: "python313".into(),
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TemplateFile {
    pub path: PathBuf,
    pub contents: String,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TemplateProgram {
    Python,
    Node,
    Npm,
    External(PathBuf),
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TemplateRun {
    pub name: String,
    pub program: TemplateProgram,
    pub args: Vec<String>,
    pub environment: BTreeMap<String, String>,
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TemplatePlan {
    pub files: Vec<TemplateFile>,
    pub directories: Vec<PathBuf>,
    pub runs: Vec<TemplateRun>,
    pub entry_files: Vec<PathBuf>,
}
impl TemplatePlan {
    fn file(&mut self, path: impl Into<PathBuf>, contents: impl Into<String>) {
        self.files.push(TemplateFile {
            path: path.into(),
            contents: contents.into(),
        });
    }
    fn run(&mut self, name: &str, program: TemplateProgram, args: &[&str]) {
        self.runs.push(TemplateRun {
            name: name.into(),
            program,
            args: args.iter().map(|s| s.to_string()).collect(),
            environment: BTreeMap::new(),
        });
    }
    fn python(&mut self, name: &str, args: &[&str]) {
        self.run(name, TemplateProgram::Python, args);
    }
    fn requirements(&mut self, requirements: &str) {
        self.file("requirements.txt", requirements);
    }
    pub fn validate(&self) -> Result<(), String> {
        let mut files = BTreeSet::new();
        for file in &self.files {
            relative(&file.path)?;
            if !files.insert(file.path.clone()) {
                return Err(format!("Duplicate template path: {}", file.path.display()));
            }
        }
        for path in self.directories.iter().chain(&self.entry_files) {
            relative(path)?;
        }
        for file in &self.files {
            for parent in file.path.ancestors().skip(1) {
                if files.contains(parent) {
                    return Err(format!(
                        "Template file is also a directory: {}",
                        parent.display()
                    ));
                }
            }
        }
        for directory in &self.directories {
            for ancestor in directory.ancestors() {
                if files.contains(ancestor) {
                    return Err(format!(
                        "Template file is also a directory: {}",
                        ancestor.display()
                    ));
                }
            }
        }
        for entry in &self.entry_files {
            if !files.contains(entry) {
                return Err(format!("Missing template entry file: {}", entry.display()));
            }
        }
        Ok(())
    }
    /// Preflight the entire plan before writing. Existing files are never
    /// overwritten; symlinks in roots, parents and targets are rejected.
    pub fn write_new(&self, root: &Path) -> io::Result<()> {
        self.validate()
            .map_err(|message| io::Error::new(io::ErrorKind::InvalidInput, message))?;
        reject_symlinks(root)?;
        if !root.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "Project root must already be a directory",
            ));
        }
        let mut directories: BTreeSet<PathBuf> = BTreeSet::new();
        for directory in &self.directories {
            for ancestor in directory.ancestors().filter(|p| !p.as_os_str().is_empty()) {
                directories.insert(ancestor.to_path_buf());
            }
        }
        for file in &self.files {
            for parent in file
                .path
                .ancestors()
                .skip(1)
                .filter(|p| !p.as_os_str().is_empty())
            {
                directories.insert(parent.to_path_buf());
            }
        }
        for directory in &directories {
            let target = root.join(directory);
            reject_symlinks(&target)?;
            if target.exists() && !target.is_dir() {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    format!("Directory conflicts with file: {}", target.display()),
                ));
            }
        }
        for file in &self.files {
            let target = root.join(&file.path);
            reject_symlinks(&target)?;
            if target.exists() {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    format!("Template would replace {}", target.display()),
                ));
            }
        }
        // Create directories in depth order. All writes use create_new, and
        // rollback removes only files this call created on ordinary I/O errors.
        let mut directories: Vec<_> = directories.into_iter().collect();
        directories.sort_by_key(|p| p.components().count());
        let mut owned_directories = Vec::new();
        let mut owned_files = Vec::new();
        let result = (|| {
            for directory in directories {
                let target = root.join(directory);
                reject_symlinks(&target)?;
                if !target.exists() {
                    std::fs::create_dir(&target)?;
                    owned_directories.push(target);
                }
            }
            for file in &self.files {
                let target = root.join(&file.path);
                reject_symlinks(&target)?;
                let mut output = std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&target)?;
                owned_files.push(target);
                output.write_all(file.contents.as_bytes())?;
            }
            Ok(())
        })();
        if result.is_err() {
            for path in owned_files.iter().rev() {
                let _ = std::fs::remove_file(path);
            }
            for path in owned_directories.iter().rev() {
                let _ = std::fs::remove_dir(path);
            }
        }
        result
    }
}
fn reject_symlinks(path: &Path) -> io::Result<()> {
    for ancestor in path.ancestors() {
        match std::fs::symlink_metadata(ancestor) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("Template path contains symlink: {}", ancestor.display()),
                ))
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}
fn relative(path: &Path) -> Result<(), String> {
    let text = path.to_string_lossy();
    // Windows callers build these with `Path::join`, so its separator is accepted there.
    let text = if cfg!(windows) {
        std::borrow::Cow::Owned(text.replace('\\', "/"))
    } else {
        text
    };
    if text.is_empty()
        || text
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
        || text.contains('\\')
        || text.contains(':')
        || text.chars().any(char::is_control)
        || path
            .components()
            .any(|c| !matches!(c, Component::Normal(_)))
    {
        return Err(format!(
            "Expected safe project-relative path: {}",
            path.display()
        ));
    }
    Ok(())
}
fn identifier(name: &str) -> Result<(), String> {
    const KEYWORDS: &[&str] = &[
        "False", "None", "True", "and", "as", "assert", "async", "await", "break", "class",
        "continue", "def", "del", "elif", "else", "except", "finally", "for", "from", "global",
        "if", "import", "in", "is", "lambda", "nonlocal", "not", "or", "pass", "raise", "return",
        "try", "while", "with", "yield",
    ];
    if name.is_empty()
        || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        || name.starts_with(|c: char| c.is_ascii_digit())
        || KEYWORDS.contains(&name)
    {
        Err(format!("Expected a Python identifier: {name}"))
    } else {
        Ok(())
    }
}
fn py(text: &str) -> String {
    serde_json::to_string(text).expect("string serialization")
}
fn html(title: &str) -> String {
    format!("<!doctype html>\n<html lang=\"en\"><head><meta charset=\"UTF-8\"><meta name=\"viewport\" content=\"width=device-width, initial-scale=1\"><title>{title}</title></head><body><h1>{title}</h1></body></html>\n")
}

pub fn plan(kind: ProjectKind, options: &TemplateOptions) -> Result<TemplatePlan, String> {
    let mut plan = TemplatePlan::default();
    match kind {
        ProjectKind::Empty => {}
        ProjectKind::Pure => {
            if options.create_main {
                plan.file("main.py","def main():\n    print('Hello from Python!')\n\n\nif __name__ == '__main__':\n    main()\n");
                plan.python("Python", &["main.py"]);
                plan.entry_files.push("main.py".into());
            }
        }
        ProjectKind::Django => django(&mut plan, options)?,
        ProjectKind::Flask => flask(&mut plan, options)?,
        ProjectKind::FastApi => {
            let prefix = if options.fastapi_async { "async " } else { "" };
            plan.file("main.py",format!("from fastapi import FastAPI\n\napp = FastAPI()\n\n\n@app.get('/')\n{prefix}def index():\n    return {{'message': 'Hello from FastAPI!'}}\n"));
            plan.requirements("fastapi\nuvicorn[standard]\n");
            plan.python(
                "FastAPI",
                &[
                    "-m",
                    "uvicorn",
                    "main:app",
                    "--reload",
                    "--host",
                    "127.0.0.1",
                ],
            );
            plan.entry_files.push("main.py".into());
        }
        ProjectKind::Pyramid => {
            plan.file("app.py","from wsgiref.simple_server import make_server\n\nfrom pyramid.config import Configurator\nfrom pyramid.response import Response\n\n\ndef index(request):\n    return Response('Hello from Pyramid!')\n\n\ndef make_app():\n    with Configurator() as config:\n        config.add_route('index', '/')\n        config.add_view(index, route_name='index')\n        return config.make_wsgi_app()\n\n\napp = make_app()\n\nif __name__ == '__main__':\n    with make_server('127.0.0.1', 6543, app) as server:\n        server.serve_forever()\n");
            plan.requirements("pyramid\n");
            plan.python("Pyramid", &["app.py"]);
            plan.entry_files.push("app.py".into());
        }
        ProjectKind::AppEngine => {
            if !matches!(
                options.appengine_runtime.as_str(),
                "python313" | "python312" | "python311" | "python314"
            ) {
                return Err(
                    "Choose a supported Python 3 App Engine runtime (python311–python314)".into(),
                );
            }
            plan.file("main.py","import os\n\nfrom flask import Flask\n\napp = Flask(__name__)\n\n\n@app.get('/')\ndef index():\n    return 'Hello from App Engine!'\n\n\nif __name__ == '__main__':\n    app.run(host='127.0.0.1', port=int(os.environ.get('PORT', '8080')))\n");
            plan.file(
                "app.yaml",
                format!(
                    "runtime: {}\nentrypoint: gunicorn -b :$PORT main:app\n",
                    options.appengine_runtime
                ),
            );
            plan.file(".gcloudignore", ".git/\n.venv/\n__pycache__/\n*.pyc\n");
            plan.requirements("Flask\ngunicorn\n");
            plan.python("App Engine (local)", &["main.py"]);
            plan.entry_files
                .extend(["main.py".into(), "app.yaml".into()]);
        }
        ProjectKind::Web2py => web2py(&mut plan, options)?,
        ProjectKind::Scientific => {
            plan.file("analysis.py","# %%\nimport numpy as np\nimport pandas as pd\nimport matplotlib.pyplot as plt\n\n# %%\ndata = pd.DataFrame({'x': np.arange(10)})\ndata['y'] = data['x'] ** 2\nprint(data)\n\n# %%\nplt.plot(data['x'], data['y'])\nplt.xlabel('x')\nplt.ylabel('y')\nplt.show()\n");
            plan.file("analysis.ipynb",serde_json::to_string_pretty(&serde_json::json!({"cells":[{"id":"introduction","cell_type":"markdown","metadata":{},"source":["# Scientific analysis\n"]},{"id":"analysis","cell_type":"code","execution_count":null,"metadata":{},"outputs":[],"source":["import numpy as np\n","import pandas as pd\n","pd.DataFrame({'x': np.arange(10)})"]}],"metadata":{"kernelspec":{"display_name":"Python 3","language":"python","name":"python3"},"language_info":{"name":"python"}},"nbformat":4,"nbformat_minor":5})).unwrap());
            plan.requirements("jupyterlab\nipykernel\nnumpy\npandas\nmatplotlib\n");
            plan.python("JupyterLab", &["-m", "jupyterlab", "analysis.ipynb"]);
            plan.python("Scientific script", &["analysis.py"]);
            plan.entry_files
                .extend(["analysis.ipynb".into(), "analysis.py".into()]);
        }
        ProjectKind::Angular | ProjectKind::React | ProjectKind::Vue | ProjectKind::Html5 => {
            frontend(&mut plan, kind, options)?
        }
    }
    let mut directories: BTreeSet<PathBuf> = plan.directories.iter().cloned().collect();
    for file in &plan.files {
        for parent in file
            .path
            .ancestors()
            .skip(1)
            .filter(|p| !p.as_os_str().is_empty())
        {
            directories.insert(parent.to_path_buf());
        }
    }
    let explicit: Vec<_> = directories.iter().cloned().collect();
    for directory in explicit {
        for ancestor in directory
            .ancestors()
            .skip(1)
            .filter(|p| !p.as_os_str().is_empty())
        {
            directories.insert(ancestor.to_path_buf());
        }
    }
    plan.directories = directories.into_iter().collect();
    plan.validate()?;
    Ok(plan)
}

fn django(plan: &mut TemplatePlan, options: &TemplateOptions) -> Result<(), String> {
    let package = crate::setup::django_project_name(&options.project_name);
    identifier(&package)?;
    identifier(&options.django_app_name)?;
    if package == options.django_app_name
        || matches!(
            options.django_app_name.as_str(),
            "django" | "typing" | "json" | "os" | "sys"
        )
    {
        return Err("Django application name must differ from the project package and standard framework modules".into());
    }
    let app = &options.django_app_name;
    if options.django_template_engine != TemplateEngine::None {
        relative(Path::new(&options.django_templates_dir))?;
    }
    plan.file("manage.py",format!("import os\nimport sys\n\n\nif __name__ == '__main__':\n    os.environ.setdefault('DJANGO_SETTINGS_MODULE', '{package}.settings')\n    from django.core.management import execute_from_command_line\n    execute_from_command_line(sys.argv)\n"));
    plan.file(format!("{package}/__init__.py"), "");
    let mut apps = vec![
        "django.contrib.auth".to_string(),
        "django.contrib.contenttypes".into(),
        "django.contrib.sessions".into(),
        "django.contrib.messages".into(),
        "django.contrib.staticfiles".into(),
        app.clone(),
    ];
    if options.django_admin {
        apps.insert(0, "django.contrib.admin".into());
    }
    let django_backend=format!("{{'BACKEND': 'django.template.backends.django.DjangoTemplates', 'DIRS': [BASE_DIR / {}], 'APP_DIRS': True, 'OPTIONS': {{'context_processors': ['django.template.context_processors.request', 'django.contrib.auth.context_processors.auth', 'django.contrib.messages.context_processors.messages']}}}}",py(if options.django_template_engine==TemplateEngine::None {"templates"} else {&options.django_templates_dir}));
    let templates=match options.django_template_engine {
        TemplateEngine::Django=>format!("[{django_backend}]"),
        TemplateEngine::Jinja2=>format!("[{{'BACKEND': 'django.template.backends.jinja2.Jinja2', 'DIRS': [BASE_DIR / {}], 'APP_DIRS': True, 'OPTIONS': {{}}}}, {django_backend}]",py(&options.django_templates_dir)),
        TemplateEngine::None if options.django_admin=>format!("[{django_backend}]"),
        TemplateEngine::None=>"[]".into(),
    };
    plan.file(format!("{package}/settings.py"),format!(r#"import os
from pathlib import Path

BASE_DIR = Path(__file__).resolve().parent.parent
SECRET_KEY = os.environ.get('DJANGO_SECRET_KEY', 'django-insecure-development-only-change-me')
DEBUG = True
ALLOWED_HOSTS = ['localhost', '127.0.0.1', '[::1]']
INSTALLED_APPS = {apps}
MIDDLEWARE = [
    'django.middleware.security.SecurityMiddleware',
    'django.contrib.sessions.middleware.SessionMiddleware',
    'django.middleware.common.CommonMiddleware',
    'django.middleware.csrf.CsrfViewMiddleware',
    'django.contrib.auth.middleware.AuthenticationMiddleware',
    'django.contrib.messages.middleware.MessageMiddleware',
    'django.middleware.clickjacking.XFrameOptionsMiddleware',
]
ROOT_URLCONF = '{package}.urls'
TEMPLATES = {templates}
WSGI_APPLICATION = '{package}.wsgi.application'
ASGI_APPLICATION = '{package}.asgi.application'
DATABASES = {{'default': {{'ENGINE': 'django.db.backends.sqlite3', 'NAME': BASE_DIR / 'db.sqlite3'}}}}
LANGUAGE_CODE = 'en-us'
TIME_ZONE = 'UTC'
USE_I18N = True
USE_TZ = True
STATIC_URL = 'static/'
DEFAULT_AUTO_FIELD = 'django.db.models.BigAutoField'
"#,apps=serde_json::to_string(&apps).unwrap()));
    let admin_import = if options.django_admin {
        "from django.contrib import admin\n"
    } else {
        ""
    };
    let admin_route = if options.django_admin {
        "    path('admin/', admin.site.urls),\n"
    } else {
        ""
    };
    plan.file(format!("{package}/urls.py"),format!("{admin_import}from django.urls import include, path\n\nurlpatterns = [\n{admin_route}    path('', include('{app}.urls')),\n]\n"));
    for (filename, interface) in [("wsgi.py", "wsgi"), ("asgi.py", "asgi")] {
        plan.file(format!("{package}/{filename}"),format!("import os\n\nfrom django.core.{interface} import get_{interface}_application\n\nos.environ.setdefault('DJANGO_SETTINGS_MODULE', '{package}.settings')\napplication = get_{interface}_application()\n"));
    }
    plan.file(format!("{app}/__init__.py"), "");
    plan.file(format!("{app}/apps.py"),format!("from django.apps import AppConfig\n\n\nclass ApplicationConfig(AppConfig):\n    default_auto_field = 'django.db.models.BigAutoField'\n    name = '{app}'\n"));
    plan.file(
        format!("{app}/models.py"),
        "from django.db import models\n\n# Define application models here.\n",
    );
    plan.file(format!("{app}/migrations/__init__.py"), "");
    plan.file(format!("{app}/tests.py"),"from django.test import TestCase\n\n\nclass IndexTests(TestCase):\n    def test_index(self):\n        self.assertEqual(self.client.get('/').status_code, 200)\n");
    if options.django_admin {
        plan.file(
            format!("{app}/admin.py"),
            "from django.contrib import admin\n\n# Register application models here.\n",
        );
    }
    plan.file(format!("{app}/urls.py"),"from django.urls import path\nfrom . import views\n\nurlpatterns = [path('', views.index, name='index')]\n");
    let view = if options.django_template_engine == TemplateEngine::None {
        "from django.http import HttpResponse\n\n\ndef index(request):\n    return HttpResponse('Hello from Django!')\n"
    } else {
        "from django.shortcuts import render\n\n\ndef index(request):\n    return render(request, 'index.html', {'message': 'Hello from Django!'})\n"
    };
    plan.file(format!("{app}/views.py"), view);
    if options.django_template_engine != TemplateEngine::None {
        plan.file(
            Path::new(&options.django_templates_dir).join("index.html"),
            html("{{ message }}"),
        );
    }
    plan.requirements(
        if options.django_template_engine == TemplateEngine::Jinja2 {
            "Django\nJinja2\n"
        } else {
            "Django\n"
        },
    );
    plan.python(
        "Django server",
        &["manage.py", "runserver", "127.0.0.1:8000"],
    );
    plan.python("Django migrate", &["manage.py", "migrate"]);
    if options.django_admin {
        plan.python("Django create superuser", &["manage.py", "createsuperuser"]);
    }
    plan.entry_files.extend([
        "manage.py".into(),
        format!("{package}/settings.py").into(),
        format!("{app}/views.py").into(),
    ]);
    Ok(())
}

fn flask(plan: &mut TemplatePlan, options: &TemplateOptions) -> Result<(), String> {
    if options.flask_template_engine == TemplateEngine::Django {
        return Err("Flask supports Jinja2 or no template engine".into());
    }
    let (imports, response) = if options.flask_template_engine == TemplateEngine::Jinja2 {
        relative(Path::new(&options.flask_templates_dir))?;
        plan.file(
            Path::new(&options.flask_templates_dir).join("index.html"),
            html("{{ message }}"),
        );
        (
            "Flask, render_template",
            "render_template('index.html', message='Hello from Flask!')",
        )
    } else {
        ("Flask", "'Hello from Flask!'")
    };
    let folder = if options.flask_template_engine == TemplateEngine::None {
        "templates"
    } else {
        &options.flask_templates_dir
    };
    plan.file("app.py",format!("from flask import {imports}\n\napp = Flask(__name__, template_folder={})\n\n\n@app.get('/')\ndef index():\n    return {response}\n",py(folder)));
    plan.requirements("Flask\n");
    plan.python(
        "Flask server",
        &[
            "-m",
            "flask",
            "--app",
            "app",
            "run",
            "--debug",
            "--host",
            "127.0.0.1",
        ],
    );
    plan.entry_files.push("app.py".into());
    Ok(())
}

fn web2py(plan: &mut TemplatePlan, options: &TemplateOptions) -> Result<(), String> {
    let framework = options
        .web2py_framework
        .as_ref()
        .ok_or("Choose the existing web2py framework directory")?;
    if !framework.is_absolute()
        || !framework.join("web2py.py").is_file()
        || !framework.join("gluon").is_dir()
    {
        return Err(
            "web2py framework directory must be absolute and contain web2py.py and gluon/".into(),
        );
    }
    let app = crate::setup::django_project_name(&options.project_name);
    identifier(&app)?;
    for folder in [
        "models",
        "modules",
        "static",
        "languages",
        "private",
        "databases",
        "sessions",
        "errors",
        "cache",
        "uploads",
    ] {
        plan.directories
            .push(format!("applications/{app}/{folder}").into());
    }
    plan.file(
        format!("applications/{app}/controllers/default.py"),
        "def index():\n    return dict(message='Hello from web2py!')\n",
    );
    plan.file(
        format!("applications/{app}/views/default/index.html"),
        html("{{=message}}"),
    );
    plan.file("routes.py",format!("default_application = '{app}'\ndefault_controller = 'default'\ndefault_function = 'index'\n"));
    plan.file(
        "run_web2py.py",
        format!(
            r#"import os
import sys
from pathlib import Path
from wsgiref.simple_server import make_server

FRAMEWORK = Path({framework})
ROOT = Path(__file__).resolve().parent
sys.path.insert(0, str(FRAMEWORK))
sys.path.insert(0, str(FRAMEWORK / 'site-packages'))
os.environ['web2py_path'] = str(FRAMEWORK)
os.chdir(ROOT)

from gluon.settings import global_settings

global_settings.gluon_parent = str(FRAMEWORK)
global_settings.applications_parent = str(ROOT)
global_settings.web2py_runtime_handler = True

from gluon.main import wsgibase

if __name__ == '__main__':
    with make_server('127.0.0.1', 8000, wsgibase) as server:
        server.serve_forever()
"#,
            framework = py(&framework.to_string_lossy())
        ),
    );
    // web2py owns its framework dependencies; the selected interpreter must be
    // compatible with that configured installation. Nothing is downloaded here.
    plan.python("web2py server", &["run_web2py.py"]);
    plan.entry_files.extend([
        "run_web2py.py".into(),
        format!("applications/{app}/controllers/default.py").into(),
    ]);
    Ok(())
}

fn frontend(
    plan: &mut TemplatePlan,
    kind: ProjectKind,
    options: &TemplateOptions,
) -> Result<(), String> {
    let mut name = options
        .project_name
        .to_ascii_lowercase()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect::<String>();
    name = name.trim_matches('-').chars().take(100).collect();
    if name.is_empty() {
        name = "rustpy-app".into();
    }
    if kind == ProjectKind::Angular {
        plan.file("package.json",serde_json::to_string_pretty(&serde_json::json!({
            "name":name,"version":"0.1.0","private":true,
            "scripts":{"dev":"ng serve --host 127.0.0.1","build":"ng build"},
            "dependencies":{"@angular/common":"^20.0.0","@angular/compiler":"^20.0.0","@angular/core":"^20.0.0","@angular/platform-browser":"^20.0.0","rxjs":"~7.8.2","tslib":"^2.8.1","zone.js":"~0.15.1"},
            "devDependencies":{"@angular/build":"^20.0.0","@angular/cli":"^20.0.0","@angular/compiler-cli":"^20.0.0","typescript":"~5.8.3"}
        })).unwrap());
        plan.file("angular.json",serde_json::to_string_pretty(&serde_json::json!({
            "$schema":"./node_modules/@angular/cli/lib/config/schema.json","version":1,"cli":{"analytics":false},"projects":{"app":{"projectType":"application","root":"","sourceRoot":"src","prefix":"app","architect":{
                "build":{"builder":"@angular/build:application","options":{"browser":"src/main.ts","index":"src/index.html","tsConfig":"tsconfig.app.json","polyfills":["zone.js"],"styles":["src/styles.css"],"outputPath":"dist/app"}},
                "serve":{"builder":"@angular/build:dev-server","options":{"buildTarget":"app:build"}}
            }}}
        })).unwrap());
        plan.file("tsconfig.json",serde_json::to_string_pretty(&serde_json::json!({"compilerOptions":{"target":"ES2022","module":"ES2022","moduleResolution":"bundler","lib":["ES2022","dom"],"strict":true,"experimentalDecorators":true,"importHelpers":true,"skipLibCheck":true},"angularCompilerOptions":{"strictTemplates":true}})).unwrap());
        plan.file("tsconfig.app.json",serde_json::to_string_pretty(&serde_json::json!({"extends":"./tsconfig.json","compilerOptions":{"outDir":"./out-tsc/app","types":[]},"files":["src/main.ts"],"include":["src/**/*.d.ts"]})).unwrap());
        plan.file("src/index.html","<!doctype html>\n<html lang=\"en\"><head><meta charset=\"UTF-8\"><title>Angular application</title><base href=\"/\"><meta name=\"viewport\" content=\"width=device-width, initial-scale=1\"></head><body><app-root></app-root></body></html>\n");
        plan.file("src/main.ts","import { Component } from '@angular/core';\nimport { bootstrapApplication } from '@angular/platform-browser';\n\n@Component({ selector: 'app-root', standalone: true, template: '<main><h1>Hello from Angular!</h1></main>' })\nclass AppComponent {}\n\nbootstrapApplication(AppComponent).catch(console.error);\n");
        plan.file(
            "src/styles.css",
            "body { margin: 2rem; font-family: system-ui, sans-serif; }\n",
        );
        plan.entry_files
            .extend(["src/main.ts".into(), "angular.json".into()]);
    } else {
        let mut package = serde_json::json!({"name":name,"version":"0.1.0","private":true,"type":"module","scripts":{"dev":"vite --host 127.0.0.1","build":"vite build","preview":"vite preview --host 127.0.0.1"},"devDependencies":{"vite":"^8.3.3"}});
        let (title, entry, body) = match kind {
            ProjectKind::React => {
                package["dependencies"] =
                    serde_json::json!({"react":"^19.3.0","react-dom":"^19.3.0"});
                package["devDependencies"]["@vitejs/plugin-react"] = serde_json::json!("^6.1.2");
                plan.file("vite.config.js","import { defineConfig } from 'vite';\nimport react from '@vitejs/plugin-react';\n\nexport default defineConfig({ plugins: [react()] });\n");
                plan.file("src/main.jsx","import React from 'react';\nimport { createRoot } from 'react-dom/client';\nimport './style.css';\n\nfunction App() { return <main><h1>Hello from React!</h1></main>; }\ncreateRoot(document.getElementById('root')).render(<App />);\n");
                (
                    "React application",
                    "src/main.jsx",
                    "<div id=\"root\"></div>",
                )
            }
            ProjectKind::Vue => {
                package["dependencies"] = serde_json::json!({"vue":"^3.5.43"});
                package["devDependencies"]["@vitejs/plugin-vue"] = serde_json::json!("^6.0.9");
                plan.file("vite.config.js","import { defineConfig } from 'vite';\nimport vue from '@vitejs/plugin-vue';\n\nexport default defineConfig({ plugins: [vue()] });\n");
                plan.file("src/main.js","import { createApp } from 'vue';\nimport App from './App.vue';\nimport './style.css';\n\ncreateApp(App).mount('#app');\n");
                plan.file(
                    "src/App.vue",
                    "<template>\n  <main><h1>Hello from Vue!</h1></main>\n</template>\n",
                );
                ("Vue application", "src/main.js", "<div id=\"app\"></div>")
            }
            ProjectKind::Html5 => {
                plan.file("src/main.js","import './style.css';\n\ndocument.querySelector('#app').textContent = 'Hello from HTML5!';\n");
                (
                    "HTML5 application",
                    "src/main.js",
                    "<main><h1 id=\"app\"></h1></main>",
                )
            }
            _ => return Err("Unsupported frontend template".into()),
        };
        plan.file("index.html",format!("<!doctype html>\n<html lang=\"en\"><head><meta charset=\"UTF-8\"><meta name=\"viewport\" content=\"width=device-width, initial-scale=1\"><title>{title}</title></head><body>{body}<script type=\"module\" src=\"/{entry}\"></script></body></html>\n"));
        plan.file(
            "src/style.css",
            "body { margin: 2rem; font-family: system-ui, sans-serif; }\n",
        );
        plan.file(
            "package.json",
            serde_json::to_string_pretty(&package).unwrap(),
        );
        plan.entry_files.extend(["index.html".into(), entry.into()]);
    }
    plan.file(".gitignore", "node_modules/\ndist/\n.angular/\n");
    plan.run(
        "Install frontend dependencies",
        TemplateProgram::Npm,
        &["install"],
    );
    plan.run(
        "Frontend development server",
        TemplateProgram::Npm,
        &["run", "dev"],
    );
    plan.run("Build frontend", TemplateProgram::Npm, &["run", "build"]);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn contents<'a>(plan: &'a TemplatePlan, path: &str) -> &'a str {
        &plan
            .files
            .iter()
            .find(|file| file.path == Path::new(path))
            .unwrap_or_else(|| panic!("missing {path}"))
            .contents
    }
    #[test]
    fn every_named_kind_has_a_valid_real_plan_and_entry_files() {
        let framework = tempfile::tempdir().unwrap();
        std::fs::write(
            framework.path().join("web2py.py"),
            "# configured framework fixture",
        )
        .unwrap();
        std::fs::create_dir(framework.path().join("gluon")).unwrap();
        let options = TemplateOptions {
            web2py_framework: Some(framework.path().to_path_buf()),
            ..Default::default()
        };
        assert_eq!(ProjectKind::all().len(), 13);
        for kind in ProjectKind::all() {
            let plan = plan(*kind, &options).unwrap();
            plan.validate().unwrap();
            if *kind != ProjectKind::Empty {
                assert!(!plan.files.is_empty(), "{kind:?}");
                assert!(!plan.runs.is_empty(), "{kind:?}");
                assert!(!plan.entry_files.is_empty(), "{kind:?}");
            }
        }
    }
    #[test]
    fn django_options_wire_custom_app_template_backend_admin_and_routes() {
        let options = TemplateOptions {
            project_name: "hello-project".into(),
            django_app_name: "catalog".into(),
            django_admin: false,
            django_template_engine: TemplateEngine::Jinja2,
            django_templates_dir: "web/pages".into(),
            ..Default::default()
        };
        let plan = plan(ProjectKind::Django, &options).unwrap();
        let settings = contents(&plan, "hello_project/settings.py");
        assert!(
            settings.contains("django.template.backends.jinja2.Jinja2")
                && settings.contains("web/pages")
                && settings.contains("catalog")
        );
        assert!(!settings.contains("django.contrib.admin"));
        assert!(contents(&plan, "hello_project/urls.py").contains("include('catalog.urls')"));
        assert!(!contents(&plan, "hello_project/urls.py").contains("admin.site"));
        assert!(!plan
            .files
            .iter()
            .any(|f| f.path == Path::new("catalog/admin.py")));
        assert!(contents(&plan, "web/pages/index.html").contains("{{ message }}"));
        assert!(plan
            .runs
            .iter()
            .any(|run| run.args == ["manage.py", "migrate"]));
    }
    #[test]
    fn django_without_templates_or_admin_generates_a_real_http_view() {
        let options = TemplateOptions {
            django_admin: false,
            django_template_engine: TemplateEngine::None,
            ..Default::default()
        };
        let plan = plan(ProjectKind::Django, &options).unwrap();
        assert!(contents(&plan, "myproject/settings.py").contains("TEMPLATES = []"));
        assert!(contents(&plan, "app/views.py").contains("HttpResponse("));
        assert!(!plan
            .files
            .iter()
            .any(|file| file.path.starts_with("templates")));
    }
    #[test]
    fn explicit_nested_directories_and_conflicting_file_ancestors_are_validated() {
        let root = tempfile::tempdir().unwrap();
        let directories = TemplatePlan {
            directories: vec!["a/b/c".into()],
            ..Default::default()
        };
        directories.write_new(root.path()).unwrap();
        assert!(root.path().join("a/b/c").is_dir());
        let conflict = TemplatePlan {
            files: vec![TemplateFile {
                path: "a".into(),
                contents: "file".into(),
            }],
            directories: vec!["a/b".into()],
            ..Default::default()
        };
        assert!(conflict.validate().is_err());
        for path in ["../a", "a/../b", "a/./b", "a//b", "C:/a"] {
            let invalid = TemplatePlan {
                files: vec![TemplateFile {
                    path: path.into(),
                    contents: String::new(),
                }],
                ..Default::default()
            };
            assert!(invalid.validate().is_err(), "{path}");
        }
    }
    #[test]
    fn engines_and_async_options_change_actual_application_source() {
        let mut options = TemplateOptions {
            flask_templates_dir: "web/jinja".into(),
            ..Default::default()
        };
        let flask = plan(ProjectKind::Flask, &options).unwrap();
        assert!(contents(&flask, "app.py").contains("template_folder=\"web/jinja\""));
        assert!(contents(&flask, "web/jinja/index.html").contains("{{ message }}"));
        options.flask_template_engine = TemplateEngine::None;
        let plain = plan(ProjectKind::Flask, &options).unwrap();
        assert!(!contents(&plain, "app.py").contains("render_template"));
        options.fastapi_async = false;
        assert!(
            contents(&plan(ProjectKind::FastApi, &options).unwrap(), "main.py")
                .contains("\ndef index():")
        );
        options.fastapi_async = true;
        assert!(
            contents(&plan(ProjectKind::FastApi, &options).unwrap(), "main.py")
                .contains("async def index():")
        );
    }
    #[test]
    fn invalid_options_cannot_escape_or_inject_python_source() {
        for invalid in [
            "../escape",
            "/absolute",
            "C:\\escape",
            "safe/../../bad",
            "a\nb",
        ] {
            let options = TemplateOptions {
                django_templates_dir: invalid.into(),
                ..Default::default()
            };
            assert!(plan(ProjectKind::Django, &options).is_err(), "{invalid}");
        }
        for invalid in ["class", "bad.name", "x';import os", "3app"] {
            let options = TemplateOptions {
                django_app_name: invalid.into(),
                ..Default::default()
            };
            assert!(plan(ProjectKind::Django, &options).is_err());
        }
        assert!(plan(ProjectKind::Web2py, &TemplateOptions::default()).is_err());
    }
    #[test]
    fn writer_preflights_existing_files_and_never_overwrites_user_work() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("main.py"), "user work").unwrap();
        let plan = plan(ProjectKind::FastApi, &TemplateOptions::default()).unwrap();
        assert!(plan.write_new(root.path()).is_err());
        assert_eq!(
            std::fs::read_to_string(root.path().join("main.py")).unwrap(),
            "user work"
        );
        assert!(!root.path().join("requirements.txt").exists());
    }
    #[test]
    fn scientific_and_frontend_files_have_real_runnable_entries() {
        let options = TemplateOptions::default();
        let scientific = plan(ProjectKind::Scientific, &options).unwrap();
        let notebook: serde_json::Value =
            serde_json::from_str(contents(&scientific, "analysis.ipynb")).unwrap();
        assert_eq!(notebook["nbformat"], 4);
        assert!(notebook["cells"]
            .as_array()
            .unwrap()
            .iter()
            .all(|cell| cell["id"].is_string()));
        assert!(contents(&scientific, "analysis.py").contains("plt.show()"));
        for kind in [
            ProjectKind::Angular,
            ProjectKind::React,
            ProjectKind::Vue,
            ProjectKind::Html5,
        ] {
            let plan = plan(kind, &options).unwrap();
            let package: serde_json::Value =
                serde_json::from_str(contents(&plan, "package.json")).unwrap();
            assert!(package["scripts"]["dev"].is_string());
            assert!(package["scripts"]["build"].is_string());
            assert!(plan
                .runs
                .iter()
                .any(|run| run.program == TemplateProgram::Npm && run.args == ["run", "dev"]));
        }
    }
    #[test]
    #[cfg(unix)]
    fn writer_rejects_symlinked_ancestor_before_creating_any_file() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path(), root.path().join("src")).unwrap();
        let plan = TemplatePlan {
            files: vec![TemplateFile {
                path: "src/main.py".into(),
                contents: "pass".into(),
            }],
            ..Default::default()
        };
        assert!(plan.write_new(root.path()).is_err());
        assert!(!outside.path().join("main.py").exists());
    }
}

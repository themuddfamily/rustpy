//! A project using an installed (non-stub) Django from an in-project virtual
//! environment: real `Model` with a metaclass, dynamic `contribute_to_class`
//! members, `path = partial(_path, ...)` and hundreds of library modules.
use rustpy_analysis::inspections::{self, InspectionOptions, Problem};
use rustpy_semantic::db::Analysis;
use rustpy_semantic::module::ModuleResolver;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

const LIBRARY: &[(&str, &str)] = &[
    ("django/__init__.py", "VERSION = (6, 1, 2)\n"),
    ("django/db/__init__.py", "from django.db import models\n"),
    (
        "django/db/models/__init__.py",
        "from django.db.models.base import Model\nfrom django.db.models.manager import Manager\nfrom django.db.models.fields import CharField, IntegerField, ForeignKey, CASCADE\nfrom django.db.models.query_utils import Q\n",
    ),
    (
        "django/db/models/base.py",
        "from django.core import exceptions\nclass ModelBase(type):\n    def __new__(cls, name, bases, attrs, **kwargs):\n        new_class = super().__new__(cls, name, bases, attrs)\n        new_class.add_to_class('DoesNotExist', exceptions.ObjectDoesNotExist)\n        for obj_name, obj in attrs.items():\n            new_class.add_to_class(obj_name, obj)\n        return new_class\n    def add_to_class(cls, name, value):\n        if hasattr(value, 'contribute_to_class'):\n            value.contribute_to_class(cls, name)\n        else:\n            setattr(cls, name, value)\n    @property\n    def _base_manager(cls):\n        return cls._meta.base_manager\nclass Model(metaclass=ModelBase):\n    def __init__(self, *args, **kwargs):\n        self._state = kwargs.get('state')\n    def _get_pk_val(self, meta=None):\n        return getattr(self, 'id', None)\n    def _set_pk_val(self, value):\n        setattr(self, 'id', value)\n    pk = property(_get_pk_val, _set_pk_val)\n    def save(self, *args, **kwargs):\n        manager = type(self)._base_manager\n        return manager.using(None).filter(pk=self.pk).first()\n    def refresh_from_db(self, using=None, fields=None):\n        return self.__class__._base_manager.db_manager(using).get(pk=self.pk)\n",
    ),
    (
        "django/db/models/fields/__init__.py",
        "class Field:\n    def __init__(self, *args, **kwargs):\n        self.kwargs = kwargs\n    def contribute_to_class(self, cls, name):\n        setattr(cls, name, self)\nclass CharField(Field):\n    pass\nclass IntegerField(Field):\n    pass\nclass ForeignKey(Field):\n    def __init__(self, to, on_delete, **kwargs):\n        super().__init__(**kwargs)\n        self.to = to\ndef CASCADE(collector, field, sub_objs, using):\n    pass\n",
    ),
    (
        "django/db/models/query.py",
        "class QuerySet:\n    def __init__(self, model=None):\n        self.model = model\n    def all(self):\n        return self._chain()\n    def filter(self, *args, **kwargs):\n        return self._chain()\n    def using(self, alias):\n        return self._chain()\n    def _chain(self):\n        return self.__class__(self.model)\n    def get(self, *args, **kwargs):\n        clone = self.filter(*args, **kwargs)\n        return clone.first()\n    def first(self):\n        for obj in self.all():\n            return obj\n    def create(self, **kwargs):\n        obj = self.model(**kwargs)\n        obj.save()\n        return obj\n    def get_or_create(self, defaults=None, **kwargs):\n        return self.get(**kwargs), False\n",
    ),
    (
        "django/db/models/manager.py",
        "from django.db.models.query import QuerySet\nclass BaseManager:\n    @classmethod\n    def from_queryset(cls, queryset_class, class_name=None):\n        return type(class_name or 'Manager', (cls,), {})\n    def get_queryset(self):\n        return QuerySet(self.model)\n    def db_manager(self, using=None):\n        return self\n    def contribute_to_class(self, cls, name):\n        self.model = cls\n        setattr(cls, name, self)\nclass Manager(BaseManager.from_queryset(QuerySet)):\n    pass\n",
    ),
    (
        "django/db/models/query_utils.py",
        "class Q:\n    def __init__(self, *args, **kwargs):\n        self.children = list(kwargs.items())\n",
    ),
    (
        "django/core/exceptions.py",
        "class ObjectDoesNotExist(Exception):\n    pass\nclass MultipleObjectsReturned(Exception):\n    pass\n",
    ),
    ("django/core/__init__.py", ""),
    (
        "django/http/__init__.py",
        "class HttpResponse:\n    def __init__(self, content=b'', status=None):\n        self.content = content\n        self.status_code = status or 200\n",
    ),
    (
        "django/template/loader.py",
        "from django.template import engines\ndef get_template(template_name, using=None):\n    for engine in engines.all():\n        template = engine.get_template(template_name)\n        if template is not None:\n            return template\n    return engines.get(using).get_template(template_name)\ndef render_to_string(template_name, context=None, request=None, using=None):\n    template = get_template(template_name, using=using)\n    return template.render(context.get('object') if context else context, request)\n",
    ),
    (
        "django/template/__init__.py",
        "from django.template.utils import EngineHandler\nengines = EngineHandler()\n",
    ),
    (
        "django/template/utils.py",
        "class EngineHandler:\n    def __init__(self, templates=None):\n        self._engines = {}\n    def all(self):\n        return [self[alias] for alias in self._engines]\n    def get(self, alias):\n        return self._engines.get(alias)\n    def __getitem__(self, alias):\n        return self._engines.get(alias)\n",
    ),
    (
        "django/shortcuts.py",
        "from django.http import HttpResponse\nfrom django.template import loader\ndef render(request, template_name, context=None, content_type=None, status=None, using=None):\n    content = loader.render_to_string(template_name, context, request, using=using)\n    return HttpResponse(content, status=status)\ndef get_object_or_404(klass, *args, **kwargs):\n    return klass._default_manager.all().get(*args, **kwargs)\n",
    ),
    (
        "django/urls/__init__.py",
        "from django.urls.conf import include, path, re_path\n",
    ),
    (
        "django/urls/conf.py",
        "from functools import partial\nclass RoutePattern:\n    def __init__(self, route, name=None, is_endpoint=False):\n        self.route = route\nclass RegexPattern(RoutePattern):\n    pass\ndef include(arg, namespace=None):\n    return (arg, None, namespace)\ndef _path(route, view, kwargs=None, name=None, Pattern=None):\n    return (Pattern(route, name=name, is_endpoint=True), view, kwargs, name)\npath = partial(_path, Pattern=RoutePattern)\nre_path = partial(_path, Pattern=RegexPattern)\n",
    ),
    ("django/contrib/__init__.py", ""),
];

const MODELS: &str = "from django.db import models\nclass Author(models.Model):\n    name = models.CharField(max_length=50)\nclass Book(models.Model):\n    title = models.CharField(max_length=90)\n    pages = models.IntegerField()\n    author = models.ForeignKey(Author, on_delete=models.CASCADE)\n";
const VIEWS: &str = "from django.shortcuts import render, get_object_or_404\nfrom django.template.loader import render_to_string\nfrom app.models import Book\ndef index(request):\n    books = Book.objects.filter(titel='x')\n    first = Book.objects.first()\n    book = Book.objects.get(pk=1)\n    try:\n        other = get_object_or_404(Book, pk=2)\n    except Book.DoesNotExist:\n        other = None\n    html = render_to_string('row.html', {'book': first, 'other': other})\n    return render(request, 'index.html', {'books': books, 'html': html, 'id': book.id, 'pk': book.pk, 'm': Book._default_manager})\n";
const URLS: &str = "from django.urls import path, re_path\nfrom app import views\nurlpatterns = [\n    path('', views.index, name='index'),\n    re_path(r'^old/$', views.index),\n]\n";

struct Project {
    _directory: tempfile::TempDir,
    root: PathBuf,
    db: Analysis,
}

fn project() -> Project {
    let directory = tempfile::TempDir::with_prefix("django-real-install").unwrap();
    let root = directory.path().to_path_buf();
    let venv = root.join(".venv");
    let site = venv.join("lib/python3.12/site-packages");
    let write = |path: &Path, text: &str| {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    };
    write(&venv.join("pyvenv.cfg"), "home = /usr/bin\n");
    for (path, text) in LIBRARY {
        write(&site.join(path), text);
    }
    // Hundreds of further library modules, like an installed Django (~2900 files).
    let mut library = Vec::new();
    for index in 0..320 {
        let path = site.join(format!("django/contrib/extra/module_{index}.py"));
        write(
            &path,
            &format!(
                "from django.db import models\nclass Extra{index}(models.Model):\n    label = models.CharField(max_length=10)\n    def lookup(self):\n        return type(self).objects.get(pk=self.pk).label\nmodels.Model.add_to_class('extra_{index}', None)\n"
            ),
        );
        library.push(path);
    }
    write(&site.join("django/contrib/extra/__init__.py"), "");
    write(&root.join("app/__init__.py"), "");
    write(&root.join("app/models.py"), MODELS);
    write(&root.join("app/views.py"), VIEWS);
    write(&root.join("app/urls.py"), URLS);
    let db = Analysis::new(ModuleResolver::new(
        vec![root.clone()],
        vec![site.clone()],
        (3, 12),
    ));
    for path in &library {
        db.file_for_path(path).unwrap();
    }
    db.set_file_text(&root.join("app/models.py"), MODELS);
    Project {
        _directory: directory,
        root,
        db,
    }
}

fn inspect(project: &Project, name: &str, text: &str) -> (Vec<Problem>, Duration) {
    let id = project.db.set_file_text(&project.root.join(name), text);
    let file = project.db.file(id).unwrap();
    let started = Instant::now();
    let problems = inspections::inspect(&project.db, &file, &InspectionOptions::default());
    (problems, started.elapsed())
}

#[test]
fn views_calling_render_finish_and_report_orm_lookups_without_false_positives() {
    let project = project();
    let (problems, elapsed) = inspect(&project, "app/views.py", VIEWS);
    assert!(
        elapsed < Duration::from_secs(20),
        "inspection took {elapsed:?}"
    );
    let orm: Vec<_> = problems
        .iter()
        .filter(|p| p.diagnostic.code == "PyDjangoOrmLookup")
        .map(|p| p.diagnostic.message.as_str())
        .collect();
    assert_eq!(
        orm,
        ["Cannot resolve 'titel' into a field of 'Book'"],
        "{problems:?}"
    );
    let unresolved: Vec<_> = problems
        .iter()
        .filter(|p| {
            p.diagnostic.code == "PyUnresolvedReferences"
                || p.diagnostic.message.contains("not callable")
        })
        .collect();
    assert!(unresolved.is_empty(), "{unresolved:?}");
    // The analysis cache answers repeated inspections without rescanning.
    let (_, again) = inspect(&project, "app/views.py", VIEWS);
    assert!(
        again < Duration::from_secs(20),
        "second inspection took {again:?}"
    );
}

#[test]
fn partial_url_helpers_are_callable() {
    let project = project();
    let (problems, _) = inspect(&project, "app/urls.py", URLS);
    let wrong: Vec<_> = problems
        .iter()
        .filter(|p| p.diagnostic.message.contains("not callable"))
        .collect();
    assert!(wrong.is_empty(), "{wrong:?}");
}

#[test]
fn queryset_keyword_completion_is_fast_with_installed_django() {
    let project = project();
    inspect(&project, "app/views.py", VIEWS);
    let typed = VIEWS.replace("filter(titel='x')", "filter(ti");
    let id = project
        .db
        .set_file_text(&project.root.join("app/views.py"), &typed);
    let file: Arc<_> = project.db.file(id).unwrap();
    let offset = rustpy_core::TextSize::new((typed.find("filter(ti").unwrap() + 9) as u32);
    let started = Instant::now();
    let completion = rustpy_semantic::basic_completion::complete(&project.db, &file, offset);
    let elapsed = started.elapsed();
    let labels: Vec<String> = completion
        .map(|completion| {
            completion
                .items
                .into_iter()
                .map(|item| item.label.to_string())
                .collect()
        })
        .unwrap_or_default();
    assert!(labels.iter().any(|label| label == "title"), "{labels:?}");
    assert!(
        elapsed < Duration::from_secs(3),
        "completion took {elapsed:?}"
    );
}

#[test]
fn repeated_inspections_reuse_library_return_types() {
    let project = project();
    let (_, first) = inspect(&project, "app/views.py", VIEWS);
    let (hits, stores, _) = project.db.library_return_cache_stats();
    assert!(stores > 0, "library returns were not cached");
    let (_, second) = inspect(&project, "app/views.py", VIEWS);
    let (hits_again, stores_again, _) = project.db.library_return_cache_stats();
    assert!(
        hits_again > hits,
        "second run did not reuse library returns"
    );
    assert!(
        stores_again - stores <= stores / 10 + 1,
        "second run re-inferred library bodies: {stores} then {stores_again}"
    );
    // Editing a project file keeps library inference; changing a library file drops it.
    let edited = VIEWS.replace("pk=1", "pk=3");
    inspect(&project, "app/views.py", &edited);
    let (_, kept, _) = project.db.library_return_cache_stats();
    assert!(
        kept - stores_again <= stores / 10 + 1,
        "{first:?} {second:?}"
    );
    project.db.set_file_text(
        &project
            .root
            .join(".venv/lib/python3.12/site-packages/django/shortcuts.py"),
        "def render(request, template_name, context=None):\n    return 1\n",
    );
    assert_eq!(project.db.library_return_cache_stats().2, 0);
}

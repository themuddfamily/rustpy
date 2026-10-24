//! Public inspection fixtures; execution is deferred to the final feature gate.
use rustpy_analysis::inspections::{self, InspectionOptions, Problem};
use rustpy_semantic::db::Analysis;
use std::path::Path;
fn inspect(source: &str) -> Vec<Problem> {
    inspect_at("m.py", source, (3, 12))
}
fn inspect_at(name: &str, source: &str, version: (u32, u32)) -> Vec<Problem> {
    let db = Analysis::for_project(Path::new("/security-item"));
    let id = db.set_file_text(&Path::new("/security-item").join(name), source);
    let options = InspectionOptions {
        python_version: version,
        ..InspectionOptions::default()
    };
    inspections::inspect(&db, &db.file(id).unwrap(), &options)
        .into_iter()
        .filter(|p| p.diagnostic.code.starts_with("PySecurity"))
        .collect()
}
fn count(problems: &[Problem], code: &str) -> usize {
    problems
        .iter()
        .filter(|p| p.diagnostic.code == code)
        .count()
}
#[test]
fn canonical_imports_aliases_and_captured_assignments_are_security_calls() {
    let p = inspect(
        "import subprocess as sp\nfrom subprocess import run as launch\ninvoke=launch\nsp.run(command,shell=True)\nlaunch(command,shell=True)\ninvoke(command,shell=True)\n",
    );
    assert_eq!(count(&p, "PySecurityShellInjection"), 3, "{p:?}");
    let p = inspect(
        "def run(*args,**kwargs):pass\nrun(command,shell=True)\nclass Fake:\n def load(self,value):return value\nyaml=Fake()\nyaml.load(data)\ndef f(eval):\n return eval(data)\n",
    );
    assert!(p.is_empty(), "{p:?}");
}
#[test]
fn rebinding_namespace_mutation_and_project_shadowing_retire_authority() {
    let p = inspect(
        "import subprocess as sp\nsp.run=replacement\nsp.run(command,shell=True)\nfrom pickle import loads\nloads=lambda x:x\nloads(data)\n",
    );
    assert!(p.is_empty(), "{p:?}");
    let db = Analysis::for_project(Path::new("/security-item"));
    db.set_file_text(
        Path::new("/security-item/random.py"),
        "def choice(value):return value[0]\n",
    );
    let id = db.set_file_text(
        Path::new("/security-item/m.py"),
        "import random\nsecret=random.choice(values)\n",
    );
    let p = inspections::inspect(&db, &db.file(id).unwrap(), &InspectionOptions::default());
    assert_eq!(count(&p, "PySecurityRandom"), 0, "{p:?}");
}
#[test]
fn execution_deserialization_and_safe_yaml_are_distinguished() {
    let p = inspect(
        "import pickle as p\nfrom yaml import load, SafeLoader\neval(source)\nexec(source)\neval('1+2')\np.loads(data)\nload(data)\nload(data,Loader=SafeLoader)\n",
    );
    assert_eq!(count(&p, "PySecurityEval"), 2, "{p:?}");
    assert_eq!(count(&p, "PySecurityUnsafeDeserialization"), 2, "{p:?}");
    let p = inspect("import yaml\nyaml.load(data)\n");
    let problem = p
        .iter()
        .find(|p| p.diagnostic.code == "PySecurityUnsafeDeserialization")
        .unwrap();
    assert!(
        problem
            .fixes
            .iter()
            .any(|f| f.edits.iter().any(|(_, t)| t == "yaml.safe_load")),
        "{p:?}"
    );
}
#[test]
fn random_is_contextual_and_hash_nonsecurity_opt_out_is_respected() {
    let p = inspect(
        "import random as rng\nimport hashlib as h\npassword=rng.choice(alphabet)\nroll=rng.randint(1,6)\nh.md5(data)\nh.sha1(data,usedforsecurity=False)\nh.new('md5',data)\nh.sha256(data)\n",
    );
    assert_eq!(count(&p, "PySecurityRandom"), 1, "{p:?}");
    assert_eq!(count(&p, "PySecurityWeakHash"), 2, "{p:?}");
}
#[test]
fn transport_template_and_tempfile_configuration_have_safe_controls() {
    let p = inspect(
        "import requests as http\nfrom jinja2 import Environment,select_autoescape\nimport ssl\nimport tempfile\nhttp.get(url,verify=False)\nhttp.get(url,verify=True)\nEnvironment(autoescape=False)\nEnvironment(autoescape=True)\nEnvironment(autoescape=select_autoescape())\nssl._create_unverified_context()\ntempfile.mktemp()\n",
    );
    assert_eq!(count(&p, "PySecurityTls"), 2, "{p:?}");
    assert_eq!(count(&p, "PySecurityAutoescape"), 1, "{p:?}");
    assert_eq!(count(&p, "PySecurityTempfile"), 1, "{p:?}");
    let tls = p
        .iter()
        .find(|p| p.diagnostic.code == "PySecurityTls" && !p.fixes.is_empty())
        .unwrap();
    assert!(tls
        .fixes
        .iter()
        .any(|f| f.edits.iter().any(|(_, v)| v == "True")));
}
#[test]
fn real_cursor_socket_tar_receivers_have_security_context_not_method_suffixes() {
    let p = inspect(
        "import sqlite3\nimport socket\nimport tarfile\nconnection=sqlite3.connect(':memory:')\ncursor=connection.cursor()\ncursor.execute('SELECT * FROM users WHERE name=%s' % name)\ncursor.execute('SELECT * FROM users WHERE name=?',(name,))\nlistener=socket.socket()\nlistener.bind(('0.0.0.0',8000))\narchive=tarfile.open(path)\narchive.extractall(destination)\narchive.extractall(destination,filter='data')\n",
    );
    assert_eq!(count(&p, "PySecuritySqlInjection"), 1, "{p:?}");
    assert_eq!(count(&p, "PySecurityBindAll"), 1, "{p:?}");
    assert_eq!(count(&p, "PySecurityTarExtraction"), 1, "{p:?}");
    let p = inspect(
        "def use(cursor,archive,listener):\n cursor.execute(query)\n archive.extractall(path)\n listener.bind(('0.0.0.0',80))\n",
    );
    assert!(p.is_empty(), "{p:?}");
}
#[test]
fn secrets_include_defaults_keywords_and_dicts_but_not_labels_or_placeholders() {
    let p = inspect(
        "password='actual credential'\npassword_label='Password'\ntoken='<TOKEN>'\ndef connect(password='actual credential'):pass\nconnect(token='actual token')\nconfig={'client_secret':'actual credential'}\n",
    );
    assert_eq!(count(&p, "PySecurityHardcodedSecret"), 4, "{p:?}");
}
#[test]
fn django_settings_and_production_assertions_are_contextual() {
    let p = inspect_at(
        "settings.py",
        "DEBUG=True\nSECRET_KEY='unsafe credential'\nALLOWED_HOSTS=['*']\n",
        (3, 12),
    );
    assert_eq!(count(&p, "PySecurityDjango"), 2, "{p:?}");
    assert_eq!(count(&p, "PySecurityHardcodedSecret"), 1, "{p:?}");
    assert_eq!(
        count(
            &inspect("DEBUG=True\nALLOWED_HOSTS=['*']\n"),
            "PySecurityDjango"
        ),
        0
    );
    assert_eq!(
        count(&inspect("assert authenticated\n"), "PySecurityAssert"),
        1
    );
    assert_eq!(
        count(
            &inspect_at("test_validation.py", "assert valid\n", (3, 12)),
            "PySecurityAssert"
        ),
        0
    );
}
#[test]
fn python_two_input_and_tar_default_filter_version_boundaries() {
    assert_eq!(
        count(
            &inspect_at("m.py", "input('value:')\n", (2, 7)),
            "PySecurityInput"
        ),
        1
    );
    assert_eq!(count(&inspect("input('value:')\n"), "PySecurityInput"), 0);
    let source = "import tarfile\narchive=tarfile.open(path)\narchive.extractall(path)\n";
    assert_eq!(
        count(
            &inspect_at("m.py", source, (3, 14)),
            "PySecurityTarExtraction"
        ),
        0
    );
}
#[test]
fn master_switch_profiles_and_suppression_keep_security_configurable() {
    let db = Analysis::for_project(Path::new("/security-item"));
    let id = db.set_file_text(
        Path::new("/security-item/m.py"),
        "import tempfile\ntempfile.mktemp() # noqa: PySecurityTempfile\n",
    );
    let p = inspections::inspect(&db, &db.file(id).unwrap(), &InspectionOptions::default());
    assert_eq!(count(&p, "PySecurityTempfile"), 0, "{p:?}");
    let id = db.set_file_text(Path::new("/security-item/m.py"), "eval(source)\n");
    let options = InspectionOptions {
        security: false,
        ..InspectionOptions::default()
    };
    let p = inspections::inspect(&db, &db.file(id).unwrap(), &options);
    assert_eq!(count(&p, "PySecurityEval"), 0, "{p:?}");
}
#[test]
fn profile_options_control_contextual_random_and_assertion_warnings() {
    use rustpy_analysis::inspections::infrastructure::Policy;
    use rustpy_settings::inspection_profiles::{Profiles, Rule};
    let db = Analysis::for_project(Path::new("/security-item"));
    let id = db.set_file_text(
        Path::new("/security-item/m.py"),
        "import random\nroll=random.randint(1,6)\nassert roll\n",
    );
    let mut profiles = Profiles::default();
    let mut random = Rule::default();
    random.options.insert("warn_all_uses".into(), "true".into());
    profiles.ide[0]
        .inspections
        .insert("PySecurityRandom".into(), random);
    let mut assertion = Rule::default();
    assertion
        .options
        .insert("warn_assertions".into(), "false".into());
    profiles.ide[0]
        .inspections
        .insert("PySecurityAssert".into(), assertion);
    let options = InspectionOptions {
        policy: Some(
            Policy::from_settings(&profiles, Some(Path::new("/security-item")), false).unwrap(),
        ),
        ..InspectionOptions::default()
    };
    let p = inspections::inspect(&db, &db.file(id).unwrap(), &options);
    assert_eq!(count(&p, "PySecurityRandom"), 1, "{p:?}");
    assert_eq!(count(&p, "PySecurityAssert"), 0, "{p:?}");
}

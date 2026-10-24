//! Bundled `.gitignore` templates: a concise
//! set in the spirit of github/gitignore, merged into an existing file
//! without repeating patterns it already has.

use std::collections::HashSet;
use std::path::Path;

use anyhow::Context as _;

/// One bundled template; `patterns` is the body written under `# <name>`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GitignoreTemplate {
    pub name: &'static str,
    pub description: &'static str,
    pub patterns: &'static str,
}

/// The bundled templates, in picker order.
pub const TEMPLATES: &[GitignoreTemplate] = &[
    GitignoreTemplate {
        name: "Python",
        description: "Byte code, packaging, test and type-checker caches",
        patterns: "__pycache__/\n*.py[cod]\n*$py.class\n*.so\nbuild/\ndist/\n*.egg-info/\n.eggs/\n*.egg\npip-log.txt\n.pytest_cache/\n.tox/\n.nox/\n.coverage\n.coverage.*\ncoverage.xml\nhtmlcov/\n.mypy_cache/\n.ruff_cache/\n.pyre/\n.pytype/\n.hypothesis/\n",
    },
    GitignoreTemplate {
        name: "Jupyter",
        description: "Notebook checkpoints and IPython profiles",
        patterns: ".ipynb_checkpoints/\nprofile_default/\nipython_config.py\n",
    },
    GitignoreTemplate {
        name: "Django",
        description: "Local settings, SQLite database, media and static output",
        patterns: "*.log\nlocal_settings.py\ndb.sqlite3\ndb.sqlite3-journal\nmedia/\nstaticfiles/\n",
    },
    GitignoreTemplate {
        name: "Flask",
        description: "Instance folder and web assets cache",
        patterns: "instance/\n.webassets-cache\n.flaskenv\n",
    },
    GitignoreTemplate {
        name: "VirtualEnv",
        description: "Virtual environments and dotenv files",
        patterns: ".venv/\nvenv/\nenv/\nENV/\n.env\npyvenv.cfg\npip-selfcheck.json\n",
    },
    GitignoreTemplate {
        name: "RustPy / IDE project files",
        description: "Per-user IDE state; shared project settings stay committable",
        patterns: "/.rustpy/workspace.json\n/.rustpy/shelf/\n.idea/workspace.xml\n.idea/tasks.xml\n.idea/usage.statistics.xml\n.idea/shelf/\n.idea/dictionaries/\n*.iws\nout/\n",
    },
    GitignoreTemplate {
        name: "macOS",
        description: "Finder metadata and resource forks",
        patterns: ".DS_Store\n.AppleDouble\n.LSOverride\n._*\n.Spotlight-V100\n.Trashes\n",
    },
    GitignoreTemplate {
        name: "Windows",
        description: "Thumbnail caches, desktop.ini and shortcuts",
        patterns: "Thumbs.db\nThumbs.db:encryptable\nehthumbs.db\n[Dd]esktop.ini\n$RECYCLE.BIN/\n*.lnk\n",
    },
    GitignoreTemplate {
        name: "Linux",
        description: "Editor backups and trash folders",
        patterns: "*~\n.fuse_hidden*\n.directory\n.Trash-*\n.nfs*\n",
    },
    GitignoreTemplate {
        name: "VisualStudioCode",
        description: "Workspace state; shared settings stay committable",
        patterns: ".vscode/*\n!.vscode/settings.json\n!.vscode/tasks.json\n!.vscode/launch.json\n!.vscode/extensions.json\n*.code-workspace\n.history/\n",
    },
    GitignoreTemplate {
        name: "Node",
        description: "Dependencies, logs and build caches",
        patterns: "node_modules/\nnpm-debug.log*\nyarn-debug.log*\nyarn-error.log*\n.npm\n.yarn/cache\n.eslintcache\n.parcel-cache\n.next/\n.nuxt/\n",
    },
];

/// RustPy's own ignore section for a new project: everything under
/// `.rustpy/` except the files a team shares.
pub const RUSTPY_PROJECT_SETTINGS: &str = "# RustPy: shared project settings\n.rustpy/*\n!.rustpy/project.json\n!.rustpy/runConfigurations.json\n!.rustpy/dataSources.json\n!.rustpy/code-style.json\n!.rustpy/scopes.json\n!.rustpy/inspectionProfiles/\n!.rustpy/inspectionProfiles/**\n!.rustpy/codeStyles/\n!.rustpy/codeStyles/**\n!.rustpy/runConfigurations/\n!.rustpy/runConfigurations/**\n!.rustpy/dictionaries/\n!.rustpy/dictionaries/**\n";

/// The bundled template called `name` (case-insensitive).
pub fn template(name: &str) -> Option<&'static GitignoreTemplate> {
    TEMPLATES.iter().find(|t| t.name.eq_ignore_ascii_case(name))
}

/// `.gitignore` written for a new project with Git: `existing` (a project
/// template's own file, often empty) merged with the Python, VirtualEnv and
/// Node templates, then RustPy's shared-settings exceptions (last, so its `!`
/// exceptions win).
pub fn new_project_gitignore(existing: &str) -> String {
    let names = ["Python", "VirtualEnv", "Node"];
    let mut text = merge_templates(existing, names.iter().filter_map(|n| template(n))).text;
    if !text.lines().any(|line| line.trim() == ".rustpy/*") {
        if !text.is_empty() {
            if !text.ends_with('\n') {
                text.push('\n');
            }
            text.push('\n');
        }
        text.push_str(RUSTPY_PROJECT_SETTINGS);
    }
    text
}

/// What merging templates into a `.gitignore` produces.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MergedGitignore {
    /// The complete new file contents.
    pub text: String,
    /// The patterns each template added, in order; templates whose patterns
    /// were all present already are left out.
    pub added: Vec<(&'static str, Vec<String>)>,
}

impl MergedGitignore {
    pub fn added_count(&self) -> usize {
        self.added.iter().map(|(_, patterns)| patterns.len()).sum()
    }
}

/// Appends each template's patterns that `existing` (or an earlier template)
/// lacks as a `# <name>` section. Comments and blank lines in a template are
/// not patterns and never count as duplicates; `existing` is kept verbatim.
pub fn merge_templates<'a>(
    existing: &str,
    templates: impl IntoIterator<Item = &'a GitignoreTemplate>,
) -> MergedGitignore {
    let is_pattern = |line: &str| !line.is_empty() && !line.starts_with('#');
    let mut seen: HashSet<String> = existing
        .lines()
        .map(|line| line.trim().to_string())
        .filter(|line| is_pattern(line))
        .collect();
    let mut text = existing.to_string();
    let mut added = Vec::new();
    for template in templates {
        let fresh: Vec<String> = template
            .patterns
            .lines()
            .map(str::trim)
            .filter(|line| is_pattern(line))
            .filter(|line| seen.insert(line.to_string()))
            .map(str::to_string)
            .collect();
        if fresh.is_empty() {
            continue;
        }
        if !text.is_empty() {
            if !text.ends_with('\n') {
                text.push('\n');
            }
            if !text.ends_with("\n\n") {
                text.push('\n');
            }
        }
        text.push_str("# ");
        text.push_str(template.name);
        text.push('\n');
        for pattern in &fresh {
            text.push_str(pattern);
            text.push('\n');
        }
        added.push((template.name, fresh));
    }
    MergedGitignore { text, added }
}

/// Merges the named templates into `<dir>/.gitignore` (creating it) and
/// returns what was added. Unknown names are ignored.
pub fn apply_templates(dir: &Path, names: &[&str]) -> anyhow::Result<MergedGitignore> {
    let _operation = crate::RepositoryOperation::enter(dir, "Generating gitignore")?;
    let path = dir.join(".gitignore");
    let existing = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    let merged = merge_templates(&existing, names.iter().filter_map(|n| template(n)));
    if merged.text != existing {
        std::fs::write(&path, &merged.text)
            .with_context(|| format!("writing {}", path.display()))?;
    }
    Ok(merged)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn named(names: &[&str]) -> Vec<&'static GitignoreTemplate> {
        names.iter().map(|n| template(n).unwrap()).collect()
    }

    #[test]
    fn bundles_the_documented_templates_with_distinct_patterns() {
        for name in [
            "Python",
            "Jupyter",
            "Django",
            "Flask",
            "VirtualEnv",
            "RustPy / IDE project files",
            "macOS",
            "Windows",
            "Linux",
            "VisualStudioCode",
            "Node",
        ] {
            let t = template(name).unwrap_or_else(|| panic!("{name}"));
            let lines: Vec<_> = t.patterns.lines().collect();
            assert!(!lines.is_empty(), "{name}");
            let unique: HashSet<_> = lines.iter().collect();
            assert_eq!(unique.len(), lines.len(), "{name} repeats a pattern");
        }
        assert!(template("rustpy / ide project files")
            .unwrap()
            .patterns
            .contains(".rustpy/workspace.json"));
        assert!(template("Missing").is_none());
    }

    #[test]
    fn new_file_gets_one_section_per_template() {
        let merged = merge_templates("", named(&["macOS", "Linux"]));
        assert!(merged.text.starts_with("# macOS\n.DS_Store\n"));
        assert!(merged.text.contains("\n\n# Linux\n*~\n"));
        assert!(merged.text.ends_with(".nfs*\n"));
        assert_eq!(merged.added.len(), 2);
        assert_eq!(merged.added_count(), 11);
    }

    #[test]
    fn existing_patterns_are_not_duplicated_and_existing_text_is_kept() {
        let existing = "# mine\n  __pycache__/  \n.venv/\n*.log";
        let merged = merge_templates(existing, named(&["Python", "VirtualEnv", "Django"]));
        assert!(merged.text.starts_with(existing));
        assert_eq!(merged.text.matches("__pycache__/").count(), 1);
        assert_eq!(merged.text.matches(".venv/").count(), 1);
        assert_eq!(merged.text.matches("*.log\n").count(), 1);
        assert!(merged.text.contains("*.log\n\n# Python\n*.py[cod]\n"));
        let django = &merged.added.iter().find(|(n, _)| *n == "Django").unwrap().1;
        assert!(!django.iter().any(|p| p == "*.log"));
        assert!(django.iter().any(|p| p == "db.sqlite3"));
    }

    #[test]
    fn overlapping_templates_add_a_pattern_once_and_skip_empty_sections() {
        // Flask contributes nothing new; Node selected twice adds its section once.
        let existing = "instance/\n.webassets-cache\n.flaskenv\n";
        let merged = merge_templates(existing, named(&["Flask", "Node", "Node"]));
        assert!(!merged.text.contains("# Flask"));
        assert_eq!(merged.text.matches("# Node").count(), 1);
        assert_eq!(merged.text.matches("node_modules/").count(), 1);
        assert_eq!(merged.added.len(), 1);
        // Merging again is a no-op.
        let again = merge_templates(&merged.text, named(&["Flask", "Node"]));
        assert_eq!(again.text, merged.text);
        assert!(again.added.is_empty());
    }

    #[test]
    fn apply_writes_and_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".gitignore"), "*.pyc\n").unwrap();
        let first = apply_templates(dir.path(), &["Python", "Unknown"]).unwrap();
        assert!(first.added_count() > 0);
        let text = std::fs::read_to_string(dir.path().join(".gitignore")).unwrap();
        assert!(text.starts_with("*.pyc\n\n# Python\n__pycache__/\n"));
        let second = apply_templates(dir.path(), &["Python"]).unwrap();
        assert_eq!(second.added_count(), 0);
        assert_eq!(
            std::fs::read_to_string(dir.path().join(".gitignore")).unwrap(),
            text
        );
    }

    #[test]
    fn new_project_gitignore_reuses_the_bundled_python_data() {
        let text = new_project_gitignore("");
        for pattern in template("Python").unwrap().patterns.lines() {
            assert!(text.lines().any(|l| l == pattern), "{pattern}");
        }
        assert!(text.lines().any(|l| l == ".venv/"));
        assert!(text.ends_with(RUSTPY_PROJECT_SETTINGS));
        // A project template's own .gitignore is kept and not repeated.
        let merged = new_project_gitignore("node_modules/\n.venv/");
        assert!(merged.starts_with("node_modules/\n.venv/\n\n# Python\n"));
        assert_eq!(merged.matches("node_modules/").count(), 1);
        assert_eq!(merged.matches(".rustpy/*").count(), 1);
        assert_eq!(new_project_gitignore(&merged), merged);
    }
}

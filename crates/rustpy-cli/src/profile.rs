//! Inspection profiles for `rustpy inspect`.
//!
//! RustPy's own profile is the `inspections` section of `settings.json`
//! (Settings › Editor › Inspections: one switch per group). `Project Default`
//! names it, matching the usual name of a project's default profile; `Default` is
//! the built-in defaults. XML profiles (`.idea/inspectionProfiles/*.xml`) are
//! read too: their `inspection_tool` entries switch single inspections on or off,
//! change their severity and carry PEP 8 `ignoredErrors` and unresolved
//! references `ignoredIdentifiers`.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use rustpy_core::Severity;
use rustpy_settings::{InspectionSettings, Settings};

use crate::mini_xml::{self, Tag};

pub const PROJECT_DEFAULT: &str = "Project Default";

/// The switch of Settings › Editor › Inspections that controls an inspection.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Group {
    Pep8,
    UnusedImports,
    UnusedVariables,
    /// Unresolved references, syntax errors and the like: always reported, as in
    /// the editor.
    Always,
    TypeChecker,
    Security,
    Compatibility,
    CodeSmells,
    Naming,
    MissingDocstrings,
    Grammar,
    Spelling,
}

impl Group {
    pub fn enabled_in(self, s: &InspectionSettings) -> bool {
        match self {
            Group::Pep8 => s.pep8,
            Group::UnusedImports => s.unused_imports,
            Group::UnusedVariables => s.unused_variables,
            Group::Always => true,
            Group::TypeChecker => s.type_checker,
            Group::Security => s.security,
            Group::Compatibility => s.compatibility,
            Group::CodeSmells => s.code_smells,
            Group::Naming => s.naming,
            Group::MissingDocstrings => s.missing_docstrings,
            Group::Grammar => s.grammar,
            Group::Spelling => s.spelling,
        }
    }

    fn set_in(self, s: &mut InspectionSettings, on: bool) {
        match self {
            Group::Pep8 => s.pep8 = on,
            Group::UnusedImports => s.unused_imports = on,
            Group::UnusedVariables => s.unused_variables = on,
            Group::Always | Group::TypeChecker | Group::Security | Group::CodeSmells => {}
            Group::Compatibility => s.compatibility = on,
            Group::Naming => s.naming = on,
            Group::MissingDocstrings => s.missing_docstrings = on,
            Group::Grammar => s.grammar = on,
            Group::Spelling => s.spelling = on,
        }
    }

    /// Whether this group is exactly one inspection (so an XML profile entry
    /// for it switches the whole group).
    fn single(self) -> bool {
        !matches!(
            self,
            Group::Always | Group::TypeChecker | Group::Security | Group::CodeSmells
        )
    }
}

/// The group of an inspection id (as used in reports, see `inspect::report_id`).
pub fn group_of(id: &str) -> Group {
    match id {
        "PyPep8" => Group::Pep8,
        "PyUnusedImports" => Group::UnusedImports,
        "PyUnusedLocal" => Group::UnusedVariables,
        "PyPep8Naming" => Group::Naming,
        "PyMissingOrEmptyDocstring" => Group::MissingDocstrings,
        "PyCompatibility" | "PyRelativeImport" | "PyClassCell" => Group::Compatibility,
        "Grammar" => Group::Grammar,
        "SpellCheckingInspection" => Group::Spelling,
        "PyArgumentList"
        | "PyTypeChecker"
        | "PyParameterList"
        | "PyCallingNonCallable"
        | "PyTypeHints" => Group::TypeChecker,
        _ if rustpy_analysis::inspections::infrastructure::inspection(id)
            .is_some_and(|inspection| inspection.group == "Type checking") =>
        {
            Group::TypeChecker
        }
        _ if id.starts_with("PySecurity") => Group::Security,
        "PyUnresolvedReferences"
        | "PySyntaxError"
        | "PyDocstring"
        | "PyRegex"
        | "PyNonlocalDeclaration"
        | "PyYieldOutsideFunction"
        | "PyReturnOutsideFunction"
        | "PyAwaitOutsideAsync"
        | "PyLoopControlOutsideLoop"
        | "PyGlobalUndefined"
        | "PyFromFutureImport" => Group::Always,
        _ => Group::CodeSmells,
    }
}

/// Which inspections run, and how severe their problems are.
#[derive(Clone, Debug, PartialEq)]
pub struct Profile {
    pub name: String,
    /// Where it came from, for messages.
    pub source: String,
    pub groups: InspectionSettings,
    /// Single inspections switched on or off (XML profiles).
    pub enabled: HashMap<String, bool>,
    /// Severity overrides (XML profiles).
    pub severity: HashMap<String, Severity>,
    /// PEP 8 and naming codes not reported (`E501`, `N802`, …).
    pub ignored_codes: HashSet<String>,
    /// Names whose unresolved references are not reported (`numpy.*`, `foo`).
    pub ignored_identifiers: Vec<String>,
}

impl Profile {
    pub fn from_groups(name: &str, source: &str, groups: InspectionSettings) -> Self {
        Self {
            name: name.into(),
            source: source.into(),
            groups,
            enabled: HashMap::new(),
            severity: HashMap::new(),
            ignored_codes: HashSet::new(),
            ignored_identifiers: Vec::new(),
        }
    }

    /// Whether inspection `id` reports problems.
    pub fn is_enabled(&self, id: &str) -> bool {
        match self.enabled.get(id) {
            Some(on) => {
                *on && (group_of(id) == Group::Always
                    || !group_of(id).single()
                    || group_of(id).enabled_in(&self.groups))
            }
            None => group_of(id).enabled_in(&self.groups),
        }
    }

    pub fn severity_of(&self, id: &str, default: Severity) -> Severity {
        self.severity.get(id).copied().unwrap_or(default)
    }

    /// Whether a problem of `id` with `message` is suppressed by the profile's
    /// ignore lists.
    pub fn ignores(&self, id: &str, message: &str) -> bool {
        match id {
            "PyPep8" | "PyPep8Naming" => message
                .split(|c: char| !c.is_ascii_alphanumeric())
                .any(|w| self.ignored_codes.contains(w)),
            "PyUnresolvedReferences" => {
                let names: Vec<&str> = message.split('\'').skip(1).step_by(2).collect();
                names.iter().any(|n| {
                    self.ignored_identifiers
                        .iter()
                        .any(|p| identifier_matches(p, n))
                }) || (names.len() >= 2
                    && self
                        .ignored_identifiers
                        .iter()
                        .any(|p| identifier_matches(p, &format!("{}.{}", names[1], names[0]))))
            }
            _ => false,
        }
    }
}

/// Ignored identifier patterns of an XML profile: a name, or a qualified name ending in `.*`.
fn identifier_matches(pattern: &str, name: &str) -> bool {
    match pattern.strip_suffix(".*") {
        Some(prefix) => name == prefix || name.starts_with(&format!("{prefix}.")),
        None => {
            pattern == name || name.rsplit('.').next() == Some(pattern) && !pattern.contains('.')
        }
    }
}

/// Severity names of XML profiles (`level` attributes).
pub fn parse_severity(level: &str) -> Option<Severity> {
    Some(match level.trim().to_ascii_uppercase().as_str() {
        "ERROR" => Severity::Error,
        "WARNING" | "SERVER PROBLEM" => Severity::Warning,
        "WEAK WARNING" => Severity::WeakWarning,
        "INFO" | "INFORMATION" | "TEXT ATTRIBUTES" => Severity::Info,
        "TYPO" | "GRAMMAR_ERROR" | "STYLE_SUGGESTION" => Severity::Typo,
        _ => return None,
    })
}

/// Our inspection id for an XML profile's `inspection_tool class`.
fn id_for_class(class: &str) -> String {
    match class {
        "SpellCheckingInspection" => "SpellCheckingInspection".into(),
        "GrazieInspection" | "LanguageDetectionInspection" => "Grammar".into(),
        "PyIncorrectDocstringInspection" => "PyDocstring".into(),
        "PyUnusedImportsInspection" => "PyUnusedImports".into(),
        _ => class
            .strip_suffix("Inspection")
            .unwrap_or(class)
            .to_string(),
    }
}

/// Reads an `.idea/inspectionProfiles` XML profile (`<profile>` or the project
/// component that wraps it).
pub fn parse_inspection_profile_xml(xml: &str, source: &str) -> Result<Profile, String> {
    let tags = mini_xml::tags(xml)?;
    let mut profile = Profile::from_groups("", source, InspectionSettings::default());
    let mut found = false;
    // The open `inspection_tool` id and the open list option (`ignoredErrors`, …).
    let mut tool: Option<String> = None;
    let mut list: Option<String> = None;
    let mut depth_stack: Vec<String> = Vec::new();
    for tag in &tags {
        match tag {
            Tag::Start { name, empty, .. } => {
                match name.as_str() {
                    "profile" => found = true,
                    "option"
                        if depth_stack.last().map(String::as_str) == Some("profile")
                            && tag.attr("name") == Some("myName") =>
                    {
                        profile.name = tag.attr("value").unwrap_or_default().to_string();
                    }
                    "inspection_tool" => {
                        let Some(class) = tag.attr("class") else {
                            continue;
                        };
                        let id = id_for_class(class);
                        if let Some(on) =
                            tag.attr("enabled").map(|v| v.eq_ignore_ascii_case("true"))
                        {
                            profile.enabled.insert(id.clone(), on);
                            let group = group_of(&id);
                            if group.single() {
                                group.set_in(&mut profile.groups, on);
                            }
                        }
                        if let Some(sev) = tag.attr("level").and_then(parse_severity) {
                            profile.severity.insert(id.clone(), sev);
                        }
                        if !empty {
                            tool = Some(id);
                        }
                    }
                    "option" if tool.is_some() && !empty => {
                        list = tag.attr("name").map(str::to_string)
                    }
                    "option" if tool.is_some() && list.is_some() => {
                        if let Some(v) = tag.attr("value") {
                            match (tool.as_deref(), list.as_deref()) {
                                (Some("PyPep8" | "PyPep8Naming"), Some("ignoredErrors")) => {
                                    profile.ignored_codes.insert(v.to_string());
                                }
                                (Some("PyUnresolvedReferences"), Some("ignoredIdentifiers")) => {
                                    profile.ignored_identifiers.push(v.to_string())
                                }
                                _ => {}
                            }
                        }
                    }
                    _ => {}
                }
                if !empty {
                    depth_stack.push(name.clone());
                }
            }
            Tag::End { name } => {
                depth_stack.pop();
                if name == "inspection_tool" {
                    tool = None;
                    list = None;
                } else if name == "option" && !depth_stack.iter().any(|n| n == "option") {
                    list = None;
                }
            }
        }
    }
    if !found {
        return Err("not an inspection profile (no <profile> element)".into());
    }
    if profile.name.is_empty() {
        profile.name = PROJECT_DEFAULT.into();
    }
    Ok(profile)
}

/// Reads a profile file: an XML inspection profile, a RustPy settings file (its
/// `inspections`), or a JSON object of group switches (`{"pep8": false}`).
pub fn load_profile_file(path: &Path) -> Result<Profile, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let source = path.display().to_string();
    if text.trim_start().starts_with('<') {
        let mut legacy = parse_inspection_profile_xml(&text, &source)
            .map_err(|error| format!("{source}: {error}"))?;
        let imported = legacy
            .groups
            .profiles
            .import_profile(&text)
            .map_err(|error| format!("{source}: {error}"))?;
        legacy
            .groups
            .profiles
            .ide
            .retain(|profile| profile.name != imported.name);
        legacy.name = imported.name.clone();
        legacy.groups.profiles.active = imported.name.clone();
        legacy.groups.profiles.ide.push(imported);
        return Ok(legacy);
    }
    let value: serde_json::Value = serde_json::from_str(&text)
        .map_err(|e| format!("{source}: not an XML inspection profile (.xml) or JSON: {e}"))?;
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().replace('_', " "))
        .unwrap_or_default();
    if value.get("inspections").is_some()
        || value.get("format").and_then(|f| f.as_str()) == Some("rustpy-settings")
    {
        let settings = crate::style::load_settings_file(path)?;
        return Ok(Profile::from_groups(
            PROJECT_DEFAULT,
            &source,
            settings.inspections,
        ));
    }
    let groups: InspectionSettings = serde_json::from_value(value)
        .map_err(|e| format!("{source}: not RustPy inspection settings: {e}"))?;
    Ok(Profile::from_groups(&stem, &source, groups))
}

/// Finds the profile `spec` names for `project`: a file (relative to the working
/// directory or the project), `Project Default` (the IDE settings), `Default`
/// (built-in), or `<project>/.idea/inspectionProfiles/<name>.xml`.
pub fn resolve(spec: &str, project: &Path, ide: &Settings) -> Result<Profile, String> {
    let spec_path = PathBuf::from(spec);
    for candidate in [spec_path.clone(), project.join(&spec_path)] {
        if candidate.is_file() {
            return load_profile_file(&candidate);
        }
    }
    if spec.eq_ignore_ascii_case(PROJECT_DEFAULT) || spec.eq_ignore_ascii_case("Project_Default") {
        return Ok(Profile::from_groups(
            PROJECT_DEFAULT,
            &format!(
                "{} (Settings › Editor › Inspections)",
                Settings::settings_path().display()
            ),
            ide.inspections.clone(),
        ));
    }
    if spec.eq_ignore_ascii_case("Default") {
        return Ok(Profile::from_groups(
            "Default",
            "built-in defaults",
            InspectionSettings::default(),
        ));
    }
    if ide
        .inspections
        .profiles
        .ide
        .iter()
        .chain(
            ide.inspections
                .profiles
                .project
                .get(&project.to_string_lossy().into_owned())
                .into_iter()
                .flatten(),
        )
        .any(|profile| profile.name == spec)
    {
        let mut groups = ide.inspections.clone();
        groups
            .profiles
            .project_active
            .insert(project.to_string_lossy().into_owned(), spec.into());
        return Ok(Profile::from_groups(
            spec,
            "IDE/project inspection profile",
            groups,
        ));
    }
    let file_name = format!("{}.xml", spec.replace(' ', "_"));
    let idea = project
        .join(".idea")
        .join("inspectionProfiles")
        .join(&file_name);
    if idea.is_file() {
        return load_profile_file(&idea);
    }
    Err(format!(
        "inspection profile '{spec}' not found: use 'Project Default', 'Default', a profile file, or a name from {}",
        project.join(".idea").join("inspectionProfiles").display()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registered_type_contracts_follow_the_type_checker_profile_group() {
        let mut groups = InspectionSettings {
            type_checker: true,
            code_smells: false,
            ..Default::default()
        };
        let profile = Profile::from_groups("Types", "fixture", groups.clone());
        let ids = [
            "PyTypedDict",
            "PyProperty",
            "PyIncompatibleOverride",
            "PyStringFormat",
        ];
        for id in ids {
            assert!(
                profile.is_enabled(id),
                "{id} follows type checking even when smells are off"
            );
        }
        groups.type_checker = false;
        groups.code_smells = true;
        let profile = Profile::from_groups("Smells", "fixture", groups);
        for id in ids {
            assert!(
                !profile.is_enabled(id),
                "{id} follows the disabled type-checker group"
            );
        }
    }

    const XML_PROFILE: &str = r#"<component name="InspectionProjectProfileManager">
  <profile version="1.0">
    <option name="myName" value="Strict" />
    <inspection_tool class="PyPep8Inspection" enabled="true" level="ERROR" enabled_by_default="true">
      <option name="ignoredErrors">
        <list>
          <option value="E501" />
          <option value="W291" />
        </list>
      </option>
    </inspection_tool>
    <inspection_tool class="PyPep8NamingInspection" enabled="false" level="WEAK WARNING" enabled_by_default="false" />
    <inspection_tool class="PyBroadExceptionInspection" enabled="false" level="WEAK WARNING" enabled_by_default="false" />
    <inspection_tool class="PyMissingOrEmptyDocstringInspection" enabled="true" level="WARNING" enabled_by_default="true" />
    <inspection_tool class="SpellCheckingInspection" enabled="false" level="TYPO" enabled_by_default="false" />
    <inspection_tool class="PyUnresolvedReferencesInspection" enabled="true" level="WARNING" enabled_by_default="true">
      <option name="ignoredIdentifiers">
        <list>
          <option value="numpy.*" />
          <option value="magic" />
        </list>
      </option>
    </inspection_tool>
  </profile>
</component>"#;

    #[test]
    fn reads_xml_profiles() {
        let p = parse_inspection_profile_xml(XML_PROFILE, "x.xml").unwrap();
        assert_eq!(p.name, "Strict");
        assert!(p.is_enabled("PyPep8"));
        assert_eq!(
            p.severity_of("PyPep8", Severity::WeakWarning),
            Severity::Error
        );
        assert!(!p.is_enabled("PyPep8Naming") && !p.groups.naming);
        assert!(!p.is_enabled("PyBroadException"));
        assert!(p.is_enabled("PyRedeclaration"), "other code smells stay on");
        assert!(
            p.is_enabled("PyMissingOrEmptyDocstring") && p.groups.missing_docstrings,
            "enabling a default-off inspection turns its group on"
        );
        assert!(!p.groups.spelling && !p.is_enabled("SpellCheckingInspection"));
        assert_eq!(
            p.severity_of("PyUnresolvedReferences", Severity::Error),
            Severity::Warning
        );
        assert!(p.ignores("PyPep8", "PEP 8: E501 line too long (130 > 120 characters)"));
        assert!(!p.ignores("PyPep8", "PEP 8: E302 expected 2 blank lines, got 1"));
        assert!(p.ignores("PyUnresolvedReferences", "Unresolved reference 'magic'"));
        assert!(p.ignores(
            "PyUnresolvedReferences",
            "Cannot find reference 'array' in 'numpy'"
        ));
        assert!(p.ignores(
            "PyUnresolvedReferences",
            "Unresolved reference 'numpy.linalg'"
        ));
        assert!(!p.ignores("PyUnresolvedReferences", "Unresolved reference 'other'"));
        assert!(parse_inspection_profile_xml("<code_scheme/>", "x").is_err());
    }

    #[test]
    fn groups_switch_inspections() {
        let mut groups = InspectionSettings::default();
        let p = Profile::from_groups("p", "s", groups.clone());
        assert!(
            p.is_enabled("PyPep8")
                && p.is_enabled("PyUnresolvedReferences")
                && p.is_enabled("PySecurityEval")
        );
        assert!(!p.is_enabled("PyMissingOrEmptyDocstring"), "off by default");
        groups.pep8 = false;
        groups.security = false;
        groups.unresolved_references = false;
        let p = Profile::from_groups("p", "s", groups);
        assert!(!p.is_enabled("PyPep8") && !p.is_enabled("PySecurityEval"));
        assert!(
            p.is_enabled("PyUnresolvedReferences") && p.is_enabled("PySyntaxError"),
            "always on, like the editor"
        );
    }

    #[test]
    fn resolves_names_and_files() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path();
        let mut ide = Settings::default();
        ide.inspections.pep8 = false;
        let p = resolve("Project Default", project, &ide).unwrap();
        assert_eq!(p.name, "Project Default");
        assert!(!p.groups.pep8, "the IDE's settings");
        assert!(
            resolve("Default", project, &ide).unwrap().groups.pep8,
            "built-in defaults"
        );
        std::fs::create_dir_all(project.join(".idea/inspectionProfiles")).unwrap();
        std::fs::write(
            project.join(".idea/inspectionProfiles/My_Strict.xml"),
            XML_PROFILE,
        )
        .unwrap();
        assert_eq!(resolve("My Strict", project, &ide).unwrap().name, "Strict");
        assert_eq!(
            resolve(".idea/inspectionProfiles/My_Strict.xml", project, &ide)
                .unwrap()
                .name,
            "Strict",
            "relative to the project"
        );
        let json = project.join("groups.json");
        std::fs::write(&json, r#"{"naming": false}"#).unwrap();
        let p = resolve(json.to_str().unwrap(), project, &ide).unwrap();
        assert!(!p.groups.naming && p.groups.pep8);
        let settings = project.join("settings.json");
        std::fs::write(&settings, r#"{"inspections": {"security": false}}"#).unwrap();
        assert!(
            !resolve(settings.to_str().unwrap(), project, &ide)
                .unwrap()
                .groups
                .security
        );
        assert!(resolve("Nope", project, &ide)
            .unwrap_err()
            .contains("not found"));
        std::fs::write(&json, "[1, 2]").unwrap();
        assert!(resolve(json.to_str().unwrap(), project, &ide).is_err());
    }
}

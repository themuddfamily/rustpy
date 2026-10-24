//! Shared IDE/project inspection policy. XML import is bounded and never resolves entities.
use rustpy_core::Severity;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::Path};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, Hash)]
pub enum HighlightingLevel {
    None,
    Syntax,
    #[default]
    All,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Hash)]
#[serde(default)]
pub struct Rule {
    pub enabled: bool,
    pub severity: Option<String>,
    /// Slash-normalized glob paths, relative to project root. Empty means all files.
    pub include: Vec<String>,
    pub exclude: Vec<String>,
    pub options: BTreeMap<String, String>,
}
impl Default for Rule {
    fn default() -> Self {
        Self {
            enabled: true,
            severity: None,
            include: Vec::new(),
            exclude: Vec::new(),
            options: BTreeMap::new(),
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Hash)]
#[serde(default)]
pub struct Profile {
    pub name: String,
    pub parent: Option<String>,
    pub inspections: BTreeMap<String, Rule>,
}
impl Default for Profile {
    fn default() -> Self {
        Self {
            name: "Default".into(),
            parent: None,
            inspections: BTreeMap::new(),
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Hash)]
pub struct CustomSeverity {
    pub name: String,
    pub base: Severity,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Hash)]
#[serde(default)]
pub struct Profiles {
    pub ide: Vec<Profile>,
    pub project: BTreeMap<String, Vec<Profile>>,
    pub active: String,
    pub project_active: BTreeMap<String, String>,
    pub custom_severities: Vec<CustomSeverity>,
    pub file_levels: BTreeMap<String, HighlightingLevel>,
}
impl Default for Profiles {
    fn default() -> Self {
        Self {
            ide: vec![Profile::default()],
            project: BTreeMap::new(),
            active: "Default".into(),
            project_active: BTreeMap::new(),
            custom_severities: Vec::new(),
            file_levels: BTreeMap::new(),
        }
    }
}
#[derive(Serialize, Deserialize)]
struct ProjectProfiles {
    profiles: Vec<Profile>,
    active: Option<String>,
    #[serde(default)]
    custom_severities: Vec<CustomSeverity>,
}

impl Profiles {
    /// Load shareable profiles, retaining IDE-only legacy entries until a
    /// project edit explicitly migrates them into the project directory.
    pub fn load_project(&mut self, root: &Path) -> Result<(), String> {
        let path = root
            .join(crate::PROJECT_DIR_NAME)
            .join("inspectionProfiles/profiles.json");
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.to_string()),
        };
        let stored: ProjectProfiles =
            serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
        let key = root.to_string_lossy().into_owned();
        self.project.insert(key.clone(), stored.profiles);
        if let Some(active) = stored.active {
            self.project_active.insert(key, active);
        }
        for severity in stored.custom_severities {
            self.custom_severities
                .retain(|existing| existing.name != severity.name);
            self.custom_severities.push(severity);
        }
        Ok(())
    }

    pub fn save_project(&self, root: &Path) -> Result<(), String> {
        let directory = root
            .join(crate::PROJECT_DIR_NAME)
            .join("inspectionProfiles");
        let path = directory.join("profiles.json");
        // A corrupt team file must be repaired explicitly, never replaced by
        // the fallback state from an earlier settings session.
        match std::fs::read(&path) {
            Ok(bytes) => {
                serde_json::from_slice::<ProjectProfiles>(&bytes)
                    .map_err(|error| error.to_string())?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.to_string()),
        }
        let key = root.to_string_lossy().into_owned();
        let stored = ProjectProfiles {
            profiles: self.project.get(&key).cloned().unwrap_or_default(),
            active: self.project_active.get(&key).cloned(),
            custom_severities: self.custom_severities.clone(),
        };
        std::fs::create_dir_all(directory).map_err(|error| error.to_string())?;
        let stage = path.with_extension("json.tmp");
        std::fs::write(
            &stage,
            serde_json::to_vec_pretty(&stored).map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        std::fs::rename(stage, path).map_err(|error| error.to_string())
    }

    pub fn resolved(&self, root: Option<&Path>) -> Result<Profile, String> {
        let key = root.map(|path| path.to_string_lossy().into_owned());
        let project = key.as_ref().and_then(|key| self.project.get(key));
        let find = |name: &str| {
            project
                .and_then(|profiles| profiles.iter().find(|profile| profile.name == name))
                .or_else(|| self.ide.iter().find(|profile| profile.name == name))
        };
        let name = key
            .as_ref()
            .and_then(|key| self.project_active.get(key))
            .unwrap_or(&self.active);
        let mut chain = Vec::new();
        let mut next = Some(name.as_str());
        while let Some(name) = next {
            if chain.len() >= 32 || chain.iter().any(|profile: &&Profile| profile.name == name) {
                return Err("Inspection profile inheritance cycle or depth limit".into());
            }
            let profile =
                find(name).ok_or_else(|| format!("Unknown inspection profile: {name}"))?;
            chain.push(profile);
            next = profile.parent.as_deref();
        }
        let mut result = Profile {
            name: name.clone(),
            ..Profile::default()
        };
        for profile in chain.into_iter().rev() {
            result.inspections.extend(profile.inspections.clone());
        }
        Ok(result)
    }
    pub fn severity(&self, name: &str) -> Option<Severity> {
        match name.to_ascii_lowercase().replace('_', " ").as_str() {
            "error" | "server problem" => Some(Severity::Error),
            "warning" => Some(Severity::Warning),
            "weak warning" => Some(Severity::WeakWarning),
            "typo" => Some(Severity::Typo),
            "information" | "info" => Some(Severity::Info),
            _ => self
                .custom_severities
                .iter()
                .find(|severity| severity.name == name)
                .map(|severity| severity.base),
        }
    }
    pub fn level(&self, path: &Path) -> HighlightingLevel {
        self.file_levels
            .get(path.to_string_lossy().as_ref())
            .copied()
            .unwrap_or_default()
    }
}
impl Profiles {
    pub fn export_profile(&self, profile: &Profile) -> String {
        let mut xml = profile.to_xml();
        let mut severities = String::new();
        for severity in &self.custom_severities {
            severities.push_str(&format!(
                "  <severity name=\"{}\" base=\"{}\"/>\n",
                escape(&severity.name),
                severity.base.label()
            ));
        }
        xml = xml.replace("</profile>", &format!("{severities}</profile>"));
        xml
    }
    pub fn import_profile(&mut self, xml: &str) -> Result<Profile, String> {
        let profile = Profile::from_xml(xml)?;
        let mut custom = self.custom_severities.clone();
        for tag in xml_tags(xml)?
            .into_iter()
            .filter(|tag| tag.split_whitespace().next() == Some("severity"))
        {
            let attributes = attributes(tag)?;
            let name = attributes.get("name").ok_or("Missing severity name")?;
            if name.trim().is_empty()
                || name.len() > 128
                || Profiles::default().severity(name).is_some()
            {
                return Err("Custom severity must have a unique non-builtin name".into());
            }
            let base = attributes
                .get("base")
                .and_then(|base| Profiles::default().severity(base))
                .ok_or("Unknown severity base")?;
            custom.retain(|severity| severity.name != *name);
            custom.push(CustomSeverity {
                name: name.clone(),
                base,
            });
        }
        self.custom_severities = custom;
        Ok(profile)
    }
    pub fn import_file(&mut self, path: &Path) -> Result<Profile, String> {
        use std::io::Read;
        let mut xml = String::new();
        std::fs::File::open(path)
            .map_err(|error| error.to_string())?
            .take(1024 * 1024 + 1)
            .read_to_string(&mut xml)
            .map_err(|error| error.to_string())?;
        self.import_profile(&xml)
    }
}

impl Rule {
    pub fn matches(&self, path: &str) -> bool {
        (self.include.is_empty() || self.include.iter().any(|pattern| glob(pattern, path)))
            && !self.exclude.iter().any(|pattern| glob(pattern, path))
    }
}
/// '*' matches within one segment; '**' crosses directories, '?' one scalar.
pub fn glob(pattern: &str, path: &str) -> bool {
    if pattern.len() > 4096 || path.len() > 16384 {
        return false;
    }
    let p: Vec<_> = pattern.chars().collect();
    let s: Vec<_> = path.chars().collect();
    if p.len().saturating_mul(s.len()) > 1_000_000 {
        return false;
    }
    let mut previous = vec![false; s.len() + 1];
    previous[0] = true;
    let mut index = 0;
    while index < p.len() {
        let mut current = vec![false; s.len() + 1];
        if p[index] == '*' {
            let cross = p.get(index + 1) == Some(&'*');
            if cross {
                index += 1;
            }
            current[0] = previous[0];
            if cross && p.get(index + 1) == Some(&'/') {
                let mut reachable = previous[0];
                for col in 1..=s.len() {
                    current[col] = previous[col] || (reachable && s[col - 1] == '/');
                    reachable |= previous[col];
                }
                index += 1;
            } else {
                for col in 1..=s.len() {
                    current[col] =
                        previous[col] || (current[col - 1] && (cross || s[col - 1] != '/'));
                }
            }
        } else {
            for col in 1..=s.len() {
                current[col] = previous[col - 1]
                    && (p[index] == s[col - 1] || (p[index] == '?' && s[col - 1] != '/'));
            }
        }
        previous = current;
        index += 1;
    }
    previous[s.len()]
}
fn escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}
fn unescape(value: &str) -> Result<String, String> {
    let mut result = String::new();
    let mut rest = value;
    while let Some(at) = rest.find('&') {
        result.push_str(&rest[..at]);
        rest = &rest[at + 1..];
        let end = rest
            .find(';')
            .filter(|end| *end <= 16)
            .ok_or("Invalid XML entity")?;
        let entity = &rest[..end];
        let character = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            _ if entity.starts_with("#x") || entity.starts_with("#X") => {
                u32::from_str_radix(&entity[2..], 16)
                    .ok()
                    .and_then(char::from_u32)
            }
            _ if entity.starts_with('#') => entity[1..].parse().ok().and_then(char::from_u32),
            _ => None,
        }
        .ok_or("Unsupported XML entity")?;
        result.push(character);
        rest = &rest[end + 1..];
    }
    result.push_str(rest);
    Ok(result)
}
fn xml_tags(xml: &str) -> Result<Vec<&str>, String> {
    if xml.len() > 1024 * 1024 {
        return Err("Inspection XML exceeds limit".into());
    }
    let mut rest = xml;
    let mut tags = Vec::new();
    while let Some(start) = rest.find('<') {
        rest = &rest[start..];
        if rest.starts_with("<!--") {
            let end = rest.find("-->").ok_or("Unterminated XML comment")?;
            rest = &rest[end + 3..];
            continue;
        }
        if rest.starts_with("<?") {
            let end = rest.find("?>").ok_or("Unterminated XML instruction")?;
            rest = &rest[end + 2..];
            continue;
        }
        if rest.starts_with("<!") {
            return Err("XML declarations and external entities are unsupported".into());
        }
        let mut quote = None;
        let mut end = None;
        for (index, character) in rest.char_indices().skip(1) {
            if let Some(current) = quote {
                if current == character {
                    quote = None;
                }
            } else if character == '\'' || character == '"' {
                quote = Some(character);
            } else if character == '>' {
                end = Some(index);
                break;
            }
        }
        let end = end.ok_or("Unclosed XML tag")?;
        if tags.len() >= 16384 {
            return Err("Too many XML elements".into());
        }
        tags.push(rest[1..end].trim());
        rest = &rest[end + 1..];
    }
    Ok(tags)
}
fn attributes(tag: &str) -> Result<BTreeMap<String, String>, String> {
    let mut attributes = BTreeMap::new();
    let mut text = tag.trim_start();
    text = text.trim_start_matches(|c: char| !c.is_whitespace());
    while !text.trim().is_empty() {
        text = text.trim_start().trim_end_matches('/').trim_end();
        if text.is_empty() {
            break;
        }
        let end = text.find('=').ok_or("Missing XML attribute value")?;
        let name = text[..end].trim();
        if name.is_empty() || name.contains(char::is_whitespace) {
            return Err("Invalid XML attribute".into());
        }
        text = text[end + 1..].trim_start();
        let quote = text.chars().next().ok_or("Missing XML quote")?;
        if quote != '\'' && quote != '"' {
            return Err("Unquoted XML attribute".into());
        }
        text = &text[1..];
        let end = text.find(quote).ok_or("Unterminated XML attribute")?;
        if attributes
            .insert(name.to_owned(), unescape(&text[..end])?)
            .is_some()
        {
            return Err("Duplicate XML attribute".into());
        }
        text = &text[end + 1..];
    }
    Ok(attributes)
}
impl Profile {
    pub fn to_xml(&self) -> String {
        let mut xml = format!(
            "<profile version=\"1.0\" name=\"{}\"{}>\n",
            escape(&self.name),
            self.parent
                .as_ref()
                .map(|parent| format!(" parent=\"{}\"", escape(parent)))
                .unwrap_or_default()
        );
        for (id, rule) in &self.inspections {
            xml.push_str(&format!(
                "  <inspection_tool class=\"{}\" enabled=\"{}\" level=\"{}\">\n",
                escape(id),
                rule.enabled,
                escape(rule.severity.as_deref().unwrap_or("DEFAULT"))
            ));
            for pattern in &rule.include {
                xml.push_str(&format!("    <scope include=\"{}\"/>\n", escape(pattern)));
            }
            for pattern in &rule.exclude {
                xml.push_str(&format!("    <scope exclude=\"{}\"/>\n", escape(pattern)));
            }
            for (name, value) in &rule.options {
                xml.push_str(&format!(
                    "    <option name=\"{}\" value=\"{}\"/>\n",
                    escape(name),
                    escape(value)
                ));
            }
            xml.push_str("  </inspection_tool>\n");
        }
        xml.push_str("</profile>\n");
        xml
    }
    pub fn from_xml(xml: &str) -> Result<Self, String> {
        let mut profile = Profile::default();
        let mut current = None;
        let mut tools = 0;
        let mut seen_profile = false;
        let mut stack: Vec<String> = Vec::new();
        for tag in xml_tags(xml)? {
            let name = tag
                .split_whitespace()
                .next()
                .unwrap_or("")
                .trim_end_matches('/');
            if let Some(closing) = name.strip_prefix('/') {
                if stack.pop().as_deref() != Some(closing) {
                    return Err("Mismatched XML closing tag".into());
                }
                if name == "/inspection_tool" {
                    current = None;
                }
                continue;
            }
            if !tag.ends_with('/') {
                if stack.len() >= 128 {
                    return Err("XML nesting limit".into());
                }
                stack.push(name.into());
            }
            let attrs = attributes(tag)?;
            match name {
                "profile" => {
                    if seen_profile {
                        return Err("Multiple profiles are not supported".into());
                    }
                    seen_profile = true;
                    profile.name = attrs
                        .get("name")
                        .cloned()
                        .unwrap_or_else(|| "Imported".into());
                    profile.parent = attrs.get("parent").cloned();
                }
                "inspection_tool" => {
                    tools += 1;
                    if tools > 4096 {
                        return Err("Too many inspection entries".into());
                    }
                    let mut id = attrs.get("class").ok_or("Missing inspection id")?.clone();
                    if let Some(short) = id.strip_suffix("Inspection") {
                        if short.starts_with("Py") {
                            id = short.into();
                        }
                    }
                    let enabled = match attrs.get("enabled").map(String::as_str) {
                        Some("false") => false,
                        Some("true") | None => true,
                        _ => return Err("Invalid inspection enabled flag".into()),
                    };
                    let severity = attrs
                        .get("level")
                        .filter(|level| level.as_str() != "DEFAULT")
                        .cloned();
                    profile.inspections.insert(
                        id.clone(),
                        Rule {
                            enabled,
                            severity,
                            ..Rule::default()
                        },
                    );
                    current = Some(id);
                }
                "option" => {
                    if current.is_none() && attrs.get("name").map(String::as_str) == Some("myName")
                    {
                        if let Some(name) = attrs.get("value") {
                            profile.name = name.clone();
                        }
                    }
                    if let Some(id) = &current {
                        if let (Some(name), Some(value)) = (attrs.get("name"), attrs.get("value")) {
                            profile
                                .inspections
                                .get_mut(id)
                                .unwrap()
                                .options
                                .insert(name.clone(), value.clone());
                        }
                    }
                }
                "scope" => {
                    if let Some(id) = &current {
                        let rule = profile.inspections.get_mut(id).unwrap();
                        if let Some(pattern) = attrs.get("include") {
                            rule.include.push(pattern.clone());
                        }
                        if let Some(pattern) = attrs.get("exclude") {
                            rule.exclude.push(pattern.clone());
                        }
                    }
                }
                _ => {}
            }
            if name == "inspection_tool" && tag.ends_with('/') {
                current = None;
            }
        }
        if !stack.is_empty() {
            return Err("Unclosed XML element".into());
        }
        if !seen_profile {
            return Err("No inspection profile in XML".into());
        }
        if profile.name.len() > 256
            || profile
                .parent
                .as_ref()
                .is_some_and(|parent| parent.len() > 256)
        {
            return Err("Profile name exceeds limit".into());
        }
        if profile.name.trim().is_empty() {
            return Err("Empty profile name".into());
        }
        Ok(profile)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn project_profiles_are_portable_and_corruption_is_not_overwritten() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        let key = root.to_string_lossy().into_owned();
        let mut profiles = Profiles::default();
        profiles.project.insert(
            key.clone(),
            vec![Profile {
                name: "Team".into(),
                ..Default::default()
            }],
        );
        profiles.project_active.insert(key.clone(), "Team".into());
        profiles.save_project(root).unwrap();
        let path = root.join(".rustpy/inspectionProfiles/profiles.json");
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains(&key));
        let mut restored = Profiles::default();
        restored.load_project(root).unwrap();
        assert_eq!(restored.resolved(Some(root)).unwrap().name, "Team");
        std::fs::write(&path, "broken team profile").unwrap();
        assert!(restored.load_project(root).is_err());
        assert!(restored.save_project(root).is_err());
        assert_eq!(
            std::fs::read_to_string(path).unwrap(),
            "broken team profile"
        );
    }

    #[test]
    fn inherited_project_override_and_cycles() {
        let mut profiles = Profiles::default();
        profiles.ide[0].inspections.insert(
            "PyTypeChecker".into(),
            Rule {
                enabled: false,
                ..Rule::default()
            },
        );
        let child = Profile {
            name: "Project".into(),
            parent: Some("Default".into()),
            inspections: BTreeMap::new(),
        };
        profiles.project.insert("/p".into(), vec![child]);
        profiles
            .project_active
            .insert("/p".into(), "Project".into());
        assert!(
            !profiles
                .resolved(Some(Path::new("/p")))
                .unwrap()
                .inspections["PyTypeChecker"]
                .enabled
        );
        profiles.ide[0].parent = Some("Project".into());
        assert!(profiles.resolved(Some(Path::new("/p"))).is_err());
    }
    #[test]
    fn xml_round_trip_and_hostile_entities() {
        let mut p = Profile {
            name: "A<&\"".into(),
            ..Profile::default()
        };
        p.inspections.insert(
            "PyTypeChecker".into(),
            Rule {
                include: vec!["src/**".into()],
                severity: Some("Server problem".into()),
                ..Rule::default()
            },
        );
        assert_eq!(Profile::from_xml(&p.to_xml()).unwrap(), p);
        assert!(Profile::from_xml("<!DOCTYPE p SYSTEM 'x'><profile/>").is_err());
        let commented = "<!-- safe > comment --><profile><option name=\"myName\" value=\"Team &amp; co\"/><inspection_tool class=\"PyUnusedImportsInspection\" enabled=\"true\"/></profile>";
        assert_eq!(Profile::from_xml(commented).unwrap().name, "Team & co");
        assert!(Profile::from_xml("<profile><inspection_tool></profile>").is_err());
        assert!(glob("src/**/*.py", "src/main.py"));
        assert!(glob("src/**/*.py", "src/a/main.py"));
        assert!(glob("src/**", "src/a/b.py"));
        assert!(!glob("src/*", "src/a/b.py"));
    }
}

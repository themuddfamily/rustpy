//! Browser profiles and live-preview preferences. Executables are invoked as argv, never shell text.
use serde::{Deserialize, Serialize};
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Kind {
    #[default]
    System,
    Chrome,
    Firefox,
    Edge,
    Safari,
    Custom,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Profile {
    pub id: String,
    pub name: String,
    pub kind: Kind,
    pub program: String,
    pub arguments: String,
    pub enabled: bool,
}
impl Default for Profile {
    fn default() -> Self {
        Self {
            id: "custom".into(),
            name: "Custom".into(),
            kind: Kind::Custom,
            program: String::new(),
            arguments: "$URL$".into(),
            enabled: true,
        }
    }
}
impl Profile {
    pub fn template(kind: Kind) -> Self {
        let name = match kind {
            Kind::System => "System",
            Kind::Chrome => "Chrome",
            Kind::Firefox => "Firefox",
            Kind::Edge => "Edge",
            Kind::Safari => "Safari",
            Kind::Custom => "Custom",
        };
        let (program, arguments) = if cfg!(target_os = "macos") {
            match kind {
                Kind::Chrome => ("open", "-a 'Google Chrome' $URL$"),
                Kind::Firefox => ("open", "-a Firefox $URL$"),
                Kind::Edge => ("open", "-a 'Microsoft Edge' $URL$"),
                Kind::Safari => ("open", "-a Safari $URL$"),
                _ => ("", "$URL$"),
            }
        } else if cfg!(windows) {
            match kind {
                Kind::Chrome => ("chrome.exe", "$URL$"),
                Kind::Firefox => ("firefox.exe", "$URL$"),
                Kind::Edge => ("msedge.exe", "$URL$"),
                _ => ("", "$URL$"),
            }
        } else {
            match kind {
                Kind::Chrome => ("google-chrome", "$URL$"),
                Kind::Firefox => ("firefox", "$URL$"),
                Kind::Edge => ("microsoft-edge", "$URL$"),
                _ => ("", "$URL$"),
            }
        };
        Self {
            id: name.to_ascii_lowercase(),
            name: name.into(),
            kind,
            program: program.into(),
            arguments: arguments.into(),
            enabled: kind != Kind::Safari || cfg!(target_os = "macos"),
        }
    }
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum DefaultBrowser {
    #[default]
    System,
    Profile(String),
    BuiltIn,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub profiles: Vec<Profile>,
    pub default: DefaultBrowser,
    pub editor_bar: bool,
    pub reload_on_save: bool,
    pub javascript: bool,
    pub zoom: f64,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            profiles: [
                Kind::System,
                Kind::Chrome,
                Kind::Firefox,
                Kind::Edge,
                Kind::Safari,
            ]
            .into_iter()
            .map(Profile::template)
            .collect(),
            default: DefaultBrowser::System,
            editor_bar: true,
            reload_on_save: true,
            javascript: true,
            zoom: 1.0,
        }
    }
}

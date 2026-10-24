//! GitHub releases: parsing the Releases API JSON, release channels, choosing the
//! update to offer and the download for this platform.

use serde::Deserialize;

use crate::version::Version;

/// "Check IDE updates for": which releases count as updates.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Channel {
    /// Stable Releases: final releases only.
    #[default]
    Stable,
    /// Early Access Program: pre-releases (`-eap.N`, `-beta.N`, `-rc.N`) too.
    Eap,
}

impl Channel {
    pub const ALL: [Channel; 2] = [Channel::Stable, Channel::Eap];

    /// From the stored setting (`"release"` / `"eap"` channel codes); anything
    /// unknown means Stable.
    pub fn from_setting(value: &str) -> Channel {
        match value.trim().to_ascii_lowercase().as_str() {
            "eap" | "beta" | "early-access" => Channel::Eap,
            _ => Channel::Stable,
        }
    }

    /// The value stored in the settings.
    pub fn setting(self) -> &'static str {
        match self {
            Channel::Stable => "release",
            Channel::Eap => "eap",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Channel::Stable => "Stable Releases",
            Channel::Eap => "Early Access Program",
        }
    }

    /// Whether `release` is offered on this channel (drafts never are).
    pub fn accepts(self, release: &Release) -> bool {
        !release.draft && (self == Channel::Eap || !release.is_early_access())
    }
}

/// A published release.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Release {
    pub version: Version,
    /// The git tag, e.g. `v0.2.0`.
    pub tag: String,
    /// Release title (the tag when the release has none).
    pub name: String,
    /// Release notes (GitHub Markdown).
    pub notes: String,
    /// The release page on GitHub.
    pub page_url: String,
    /// Marked as a pre-release on GitHub.
    pub prerelease: bool,
    pub draft: bool,
    /// `2026-10-01T12:00:00Z`; empty when unknown.
    pub published_at: String,
    pub assets: Vec<Asset>,
}

/// A downloadable file of a release.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Asset {
    pub name: String,
    pub url: String,
    pub size: u64,
}

impl Release {
    /// A pre-release by its GitHub flag or by its version (`0.3.0-eap.1`).
    pub fn is_early_access(&self) -> bool {
        self.prerelease || self.version.is_prerelease()
    }

    /// The publication date, `2026-10-01`.
    pub fn date(&self) -> &str {
        self.published_at.get(..10).unwrap_or(&self.published_at)
    }
}

#[derive(Deserialize)]
struct ApiRelease {
    #[serde(default)]
    tag_name: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    html_url: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    published_at: Option<String>,
    #[serde(default)]
    assets: Vec<ApiAsset>,
}

#[derive(Deserialize)]
struct ApiAsset {
    #[serde(default)]
    name: String,
    #[serde(default)]
    browser_download_url: String,
    #[serde(default)]
    size: u64,
}

/// Parses a GitHub `GET /repos/{owner}/{repo}/releases` answer (a list) or a single release
/// object. Releases whose tag is not a version (`nightly`) are left out.
pub fn parse_releases(json: &str) -> Result<Vec<Release>, String> {
    let value: serde_json::Value =
        serde_json::from_str(json).map_err(|e| format!("not JSON: {e}"))?;
    let items: Vec<ApiRelease> = match value {
        serde_json::Value::Array(_) => {
            serde_json::from_value(value).map_err(|e| format!("unexpected release list: {e}"))?
        }
        serde_json::Value::Object(ref o) if o.contains_key("tag_name") => {
            vec![serde_json::from_value(value).map_err(|e| format!("unexpected release: {e}"))?]
        }
        serde_json::Value::Object(o) => {
            // An API error such as {"message": "Not Found", ...}.
            let message = o
                .get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("no releases in the answer");
            return Err(message.to_string());
        }
        _ => return Err("unexpected answer".into()),
    };
    Ok(items
        .into_iter()
        .filter_map(|r| {
            // Only the tag counts: titles such as "Nightly 2026.09.30" are not versions.
            let version = Version::parse(&r.tag_name)?;
            let name = r
                .name
                .filter(|n| !n.trim().is_empty())
                .unwrap_or_else(|| r.tag_name.clone());
            let assets = r
                .assets
                .into_iter()
                .filter(|a| !a.name.is_empty() && !a.browser_download_url.is_empty())
                .map(|a| Asset {
                    name: a.name,
                    url: a.browser_download_url,
                    size: a.size,
                })
                .collect();
            Some(Release {
                version,
                tag: r.tag_name,
                name,
                notes: r.body.unwrap_or_default(),
                page_url: r.html_url,
                prerelease: r.prerelease,
                draft: r.draft,
                published_at: r.published_at.unwrap_or_default(),
                assets,
            })
        })
        .collect())
}

/// The newest release on `channel`, whatever the running version.
pub fn newest(releases: &[Release], channel: Channel) -> Option<&Release> {
    releases
        .iter()
        .filter(|r| channel.accepts(r))
        .max_by(|a, b| a.version.cmp(&b.version))
}

/// The update to offer: the newest release on `channel` when it is newer than `current`
/// and not one of the `ignored` versions ("Ignore This Update"). An ignored newest
/// release means nothing is offered until a newer one appears.
pub fn update_for<'a>(
    current: &Version,
    releases: &'a [Release],
    channel: Channel,
    ignored: &[String],
) -> Option<&'a Release> {
    let candidate = newest(releases, channel).filter(|r| r.version > *current)?;
    let is_ignored = ignored.iter().any(|i| {
        i.trim() == candidate.tag || Version::parse(i).is_some_and(|v| v == candidate.version)
    });
    (!is_ignored).then_some(candidate)
}

/// Operating system of a download.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Os {
    Linux,
    Windows,
    MacOs,
    Other,
}

/// CPU architecture of a download.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Arch {
    X86_64,
    Aarch64,
    Other,
}

/// How the running IDE was installed: decides which download fits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InstallKind {
    AppImage,
    Deb,
    Rpm,
    /// Flatpak and Snap update through their stores: only the release page is offered.
    Flatpak,
    Snap,
    /// Windows installer (Inno Setup).
    Installer,
    Msi,
    /// Windows portable zip.
    Portable,
    /// macOS app bundle (from the `.dmg`).
    AppBundle,
    /// A bare binary (`.tar.gz`), a source build or unknown.
    Archive,
    /// Windows MSIX package (Microsoft Store or a sideloaded `.msix`): the package runs
    /// from the read-only `WindowsApps` folder and the Store delivers its updates.
    Store,
}

/// Shown instead of update downloads in a Microsoft Store (MSIX) installation.
pub const STORE_UPDATES_MESSAGE: &str =
    "Updates are managed by the Microsoft Store. Open the Store's Library page to get the latest RustPy.";

impl InstallKind {
    /// Stable lower-case name, as written in `rustpy-install.kind` and printed by
    /// `rustpy --verbose --version`.
    pub fn id(self) -> &'static str {
        match self {
            InstallKind::AppImage => "appimage",
            InstallKind::Deb => "deb",
            InstallKind::Rpm => "rpm",
            InstallKind::Flatpak => "flatpak",
            InstallKind::Snap => "snap",
            InstallKind::Installer => "installer",
            InstallKind::Msi => "msi",
            InstallKind::Portable => "portable",
            InstallKind::AppBundle => "app-bundle",
            InstallKind::Archive => "archive",
            InstallKind::Store => "msix",
        }
    }

    /// The install kind named by the contents of a `rustpy-install.kind` marker
    /// (`msi`, `msix` or `store`; surrounding whitespace and case are ignored).
    pub fn from_marker(text: &str) -> Option<InstallKind> {
        match text.trim().to_ascii_lowercase().as_str() {
            "msi" => Some(InstallKind::Msi),
            "msix" | "store" | "appx" => Some(InstallKind::Store),
            _ => None,
        }
    }

    /// Whether RustPy downloads and installs its own updates. Store-managed
    /// installations (Flatpak, Snap, Microsoft Store) are updated by their store.
    pub fn self_updates(self) -> bool {
        !matches!(
            self,
            InstallKind::Flatpak | InstallKind::Snap | InstallKind::Store
        )
    }

    /// Whether update checks are pointless because a store owns updates entirely
    /// (the Microsoft Store forbids side-channel update prompts).
    pub fn updates_managed_externally(self) -> bool {
        self == InstallKind::Store
    }
}

/// The Windows install kind from what can be observed: whether the process has MSIX
/// package identity, the contents of `rustpy-install.kind` beside the executable (if it
/// exists) and whether the Inno Setup uninstaller is there. Package identity wins: a
/// packaged process is always the Store kind, whatever files sit beside it.
pub fn windows_install_kind(
    package_identity: bool,
    marker: Option<&str>,
    uninstaller: bool,
) -> InstallKind {
    if package_identity {
        return InstallKind::Store;
    }
    match marker {
        // An unreadable or unknown marker is the MSI's (older MSIs wrote `msi` only).
        Some(text) => InstallKind::from_marker(text).unwrap_or(InstallKind::Msi),
        None if uninstaller => InstallKind::Installer,
        None => InstallKind::Portable,
    }
}

/// Whether this process runs with MSIX package identity (installed from the Microsoft
/// Store or a sideloaded `.msix`), by `GetCurrentPackageFullName`.
#[cfg(windows)]
pub fn has_package_identity() -> bool {
    use windows_sys::Win32::Foundation::{APPMODEL_ERROR_NO_PACKAGE, ERROR_INSUFFICIENT_BUFFER};
    use windows_sys::Win32::Storage::Packaging::Appx::GetCurrentPackageFullName;
    let mut length = 0u32;
    // SAFETY: a zero length with a null buffer only asks for the required length.
    let status = unsafe { GetCurrentPackageFullName(&mut length, std::ptr::null_mut()) };
    match status {
        ERROR_INSUFFICIENT_BUFFER => true,
        APPMODEL_ERROR_NO_PACKAGE => false,
        // ERROR_SUCCESS cannot happen with a zero-length buffer; anything else is unknown.
        other => other == 0,
    }
}

/// Without Windows there is no MSIX package identity.
#[cfg(not(windows))]
pub fn has_package_identity() -> bool {
    false
}

/// The platform a download must fit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Platform {
    pub os: Os,
    pub arch: Arch,
    pub install: InstallKind,
}

impl Platform {
    /// The running platform: OS and architecture from the build, the install kind from the
    /// environment (`APPIMAGE`, `FLATPAK_ID`, `SNAP`) and the executable's location.
    pub fn current() -> Platform {
        let os = if cfg!(target_os = "linux") {
            Os::Linux
        } else if cfg!(windows) {
            Os::Windows
        } else if cfg!(target_os = "macos") {
            Os::MacOs
        } else {
            Os::Other
        };
        let arch = match std::env::consts::ARCH {
            "x86_64" => Arch::X86_64,
            "aarch64" => Arch::Aarch64,
            _ => Arch::Other,
        };
        let exe = std::env::current_exe().ok();
        let env = |k: &str| std::env::var_os(k).is_some_and(|v| !v.is_empty());
        let install = match os {
            Os::Linux if env("APPIMAGE") => InstallKind::AppImage,
            Os::Linux if env("FLATPAK_ID") => InstallKind::Flatpak,
            Os::Linux if env("SNAP") => InstallKind::Snap,
            Os::Linux if exe.as_ref().is_some_and(|e| e.starts_with("/usr")) => {
                if std::path::Path::new("/var/lib/dpkg/info/rustpy.list").exists() {
                    InstallKind::Deb
                } else if std::path::Path::new("/var/lib/rpm").exists()
                    || std::path::Path::new("/usr/lib/sysimage/rpm").exists()
                {
                    InstallKind::Rpm
                } else {
                    InstallKind::Archive
                }
            }
            Os::Windows => {
                let dir = exe.as_ref().and_then(|e| e.parent());
                let marker = dir.and_then(|d| {
                    let path = d.join("rustpy-install.kind");
                    match std::fs::read_to_string(&path) {
                        Ok(text) => Some(text),
                        Err(_) if path.exists() => Some(String::new()),
                        Err(_) => None,
                    }
                });
                windows_install_kind(
                    has_package_identity(),
                    marker.as_deref(),
                    dir.is_some_and(|d| d.join("unins000.exe").exists()),
                )
            }
            Os::MacOs
                if exe
                    .as_ref()
                    .is_some_and(|e| e.to_string_lossy().contains(".app/Contents/MacOS/")) =>
            {
                InstallKind::AppBundle
            }
            _ => InstallKind::Archive,
        };
        Platform { os, arch, install }
    }
}

/// The download of `release` that fits `platform`, by the asset names the release workflow
/// publishes (`RustPy-0.2.0-x86_64.AppImage`, `rustpy_0.2.0-1_amd64.deb`,
/// `rustpy-0.2.0-1.x86_64.rpm`, `rustpy-0.2.0-linux-x86_64.tar.gz`,
/// `rustpy-0.2.0-windows-arm64-setup.exe`, `…-portable.zip`, `RustPy-0.2.0-universal.dmg`).
/// `None` (open the release page instead) for Flatpak and Snap installs and when nothing fits,
/// and always for Microsoft Store installs (the Store delivers their updates).
pub fn asset_for<'a>(release: &'a Release, platform: &Platform) -> Option<&'a Asset> {
    if platform.install.updates_managed_externally() {
        return None;
    }
    let arch_words: &[&str] = match platform.arch {
        Arch::X86_64 => &["x86_64", "amd64", "x64"],
        Arch::Aarch64 => &["aarch64", "arm64"],
        Arch::Other => &[],
    };
    let lower: Vec<(String, &Asset)> = release
        .assets
        .iter()
        .map(|a| (a.name.to_ascii_lowercase(), a))
        .collect();
    let find = |pred: &dyn Fn(&str) -> bool| lower.iter().find(|(n, _)| pred(n)).map(|(_, a)| *a);
    let for_arch = |n: &str| arch_words.iter().any(|w| n.contains(w));
    let linux_tarball = |n: &str| n.contains("linux") && n.ends_with(".tar.gz") && for_arch(n);
    let appimage = |n: &str| n.ends_with(".appimage") && for_arch(n);
    match platform.os {
        Os::Linux => match platform.install {
            InstallKind::Flatpak | InstallKind::Snap => None,
            InstallKind::AppImage => find(&appimage),
            InstallKind::Deb => find(&|n| n.ends_with(".deb") && for_arch(n)),
            InstallKind::Rpm => find(&|n| n.ends_with(".rpm") && for_arch(n)),
            _ => find(&linux_tarball).or_else(|| find(&appimage)),
        },
        Os::Windows => {
            let setup = |n: &str| n.ends_with("-setup.exe") && for_arch(n);
            let portable = |n: &str| n.contains("windows") && n.ends_with(".zip") && for_arch(n);
            if platform.install == InstallKind::Msi {
                find(&|n| n.ends_with(".msi") && for_arch(n))
            } else if platform.install == InstallKind::Portable {
                find(&portable).or_else(|| find(&setup))
            } else {
                find(&setup).or_else(|| find(&portable))
            }
        }
        Os::MacOs => find(&|n| n.ends_with(".dmg") && n.contains("universal"))
            .or_else(|| find(&|n| n.ends_with(".dmg") && for_arch(n)))
            .or_else(|| {
                find(&|n| {
                    n.contains("macos")
                        && n.ends_with(".tar.gz")
                        && (n.contains("universal") || for_arch(n))
                })
            }),
        Os::Other => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn release(tag: &str, prerelease: bool, draft: bool) -> Release {
        Release {
            version: Version::parse(tag).unwrap(),
            tag: tag.into(),
            name: tag.into(),
            notes: String::new(),
            page_url: format!("https://github.com/o/r/releases/tag/{tag}"),
            prerelease,
            draft,
            published_at: String::new(),
            assets: Vec::new(),
        }
    }

    #[test]
    fn channels_filter_prereleases_and_drafts() {
        let list = vec![
            release("v0.2.0", false, false),
            release("v0.3.0-eap.1", true, false),
            release("v0.3.0-beta.1", false, false),
            release("v0.4.0", false, true),
        ];
        assert_eq!(
            newest(&list, Channel::Stable).map(|r| r.tag.as_str()),
            Some("v0.2.0")
        );
        // EAP: the beta tag sorts after the EAP one ("beta" > "eap" is false: b < e).
        assert_eq!(
            newest(&list, Channel::Eap).map(|r| r.tag.as_str()),
            Some("v0.3.0-eap.1")
        );
        // A GitHub pre-release flag on a plain version still counts as early access.
        let flagged = vec![
            release("v0.5.0", true, false),
            release("v0.4.1", false, false),
        ];
        assert_eq!(
            newest(&flagged, Channel::Stable).map(|r| r.tag.as_str()),
            Some("v0.4.1")
        );
        assert_eq!(
            newest(&flagged, Channel::Eap).map(|r| r.tag.as_str()),
            Some("v0.5.0")
        );
        // A final release newer than every EAP build is offered on the EAP channel too.
        let mixed = vec![
            release("v1.0.0", false, false),
            release("v1.0.0-eap.3", true, false),
        ];
        assert_eq!(
            newest(&mixed, Channel::Eap).map(|r| r.tag.as_str()),
            Some("v1.0.0")
        );
        assert!(newest(&[], Channel::Eap).is_none());
    }

    #[test]
    fn channel_settings_round_trip() {
        for c in Channel::ALL {
            assert_eq!(Channel::from_setting(c.setting()), c);
        }
        assert_eq!(Channel::from_setting(""), Channel::Stable);
        assert_eq!(Channel::from_setting("EAP"), Channel::Eap);
        assert_eq!(Channel::from_setting("nightly"), Channel::Stable);
        assert_eq!(Channel::Eap.label(), "Early Access Program");
    }

    #[test]
    fn offers_only_newer_and_not_ignored_releases() {
        let list = vec![
            release("v0.2.0", false, false),
            release("v0.1.5", false, false),
            release("v0.3.0-eap.2", true, false),
        ];
        let current = Version::parse("0.1.5").unwrap();
        assert_eq!(
            update_for(&current, &list, Channel::Stable, &[]).map(|r| r.tag.as_str()),
            Some("v0.2.0")
        );
        assert_eq!(
            update_for(&current, &list, Channel::Eap, &[]).map(|r| r.tag.as_str()),
            Some("v0.3.0-eap.2")
        );
        // Ignoring the newest offers nothing (not the older release).
        assert!(update_for(&current, &list, Channel::Stable, &["0.2.0".into()]).is_none());
        assert!(update_for(&current, &list, Channel::Stable, &["v0.2.0".into()]).is_none());
        assert!(update_for(&current, &list, Channel::Eap, &["0.3.0-eap.2".into()]).is_none());
        assert_eq!(
            update_for(&current, &list, Channel::Eap, &["0.3.0-eap.1".into()])
                .map(|r| r.tag.as_str()),
            Some("v0.3.0-eap.2")
        );
        // Up to date, or ahead of every release (a development build).
        assert!(update_for(
            &Version::parse("0.2.0").unwrap(),
            &list,
            Channel::Stable,
            &[]
        )
        .is_none());
        assert!(update_for(
            &Version::parse("0.4.0-dev").unwrap(),
            &list,
            Channel::Eap,
            &[]
        )
        .is_none());
        // An EAP user is offered the final release of the same version.
        let current = Version::parse("0.3.0-eap.2").unwrap();
        let list = vec![release("v0.3.0", false, false)];
        assert_eq!(
            update_for(&current, &list, Channel::Stable, &[]).map(|r| r.tag.as_str()),
            Some("v0.3.0")
        );
    }

    #[test]
    fn parses_single_objects_errors_and_skips_odd_tags() {
        let one = r#"{"tag_name": "v1.0.0", "name": null, "body": null, "html_url": "https://x/r/v1.0.0", "assets": []}"#;
        let r = parse_releases(one).unwrap();
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].name, "v1.0.0");
        assert_eq!(r[0].notes, "");
        assert_eq!(r[0].date(), "");
        assert_eq!(
            parse_releases(r#"{"message": "Not Found", "documentation_url": "x"}"#),
            Err("Not Found".to_string())
        );
        assert!(parse_releases("<html>").is_err());
        assert!(parse_releases("42").is_err());
        let odd = r#"[{"tag_name": "nightly", "name": "Nightly 2026.09.30"}, {"tag_name": "release-2", "name": "RustPy 0.9.1"}, {"tag_name": "v0.9.0"}]"#;
        let r = parse_releases(odd).unwrap();
        assert_eq!(
            r.iter().map(|r| r.version.to_string()).collect::<Vec<_>>(),
            vec!["0.9.0"]
        );
    }

    fn with_assets(names: &[&str]) -> Release {
        let mut r = release("v0.2.0", false, false);
        r.assets = names
            .iter()
            .map(|n| Asset {
                name: n.to_string(),
                url: format!("https://dl/{n}"),
                size: 1,
            })
            .collect();
        r
    }

    #[test]
    fn picks_the_download_for_each_platform() {
        let r = with_assets(&[
            "SHA256SUMS",
            "rustpy-0.2.0-linux-x86_64.tar.gz",
            "rustpy-0.2.0-linux-aarch64.tar.gz",
            "RustPy-0.2.0-x86_64.AppImage",
            "RustPy-0.2.0-aarch64.AppImage",
            "rustpy_0.2.0-1_amd64.deb",
            "rustpy_0.2.0-1_arm64.deb",
            "rustpy-0.2.0-1.x86_64.rpm",
            "rustpy-0.2.0-1.aarch64.rpm",
            "rustpy-0.2.0-windows-x86_64-setup.exe",
            "rustpy-0.2.0-windows-x86_64-portable.zip",
            "rustpy-0.2.0-windows-arm64-setup.exe",
            "rustpy-0.2.0-windows-arm64-portable.zip",
            "RustPy-0.2.0-universal.dmg",
        ]);
        let pick = |os, arch, install| {
            asset_for(&r, &Platform { os, arch, install }).map(|a| a.name.as_str())
        };
        use Arch::{Aarch64, X86_64};
        use InstallKind::*;
        use Os::{Linux, MacOs, Windows};
        assert_eq!(
            pick(Linux, X86_64, AppImage),
            Some("RustPy-0.2.0-x86_64.AppImage")
        );
        assert_eq!(
            pick(Linux, Aarch64, AppImage),
            Some("RustPy-0.2.0-aarch64.AppImage")
        );
        assert_eq!(pick(Linux, X86_64, Deb), Some("rustpy_0.2.0-1_amd64.deb"));
        assert_eq!(pick(Linux, Aarch64, Deb), Some("rustpy_0.2.0-1_arm64.deb"));
        assert_eq!(pick(Linux, X86_64, Rpm), Some("rustpy-0.2.0-1.x86_64.rpm"));
        assert_eq!(
            pick(Linux, Aarch64, Rpm),
            Some("rustpy-0.2.0-1.aarch64.rpm")
        );
        assert_eq!(
            pick(Linux, X86_64, Archive),
            Some("rustpy-0.2.0-linux-x86_64.tar.gz")
        );
        assert_eq!(
            pick(Linux, Aarch64, Archive),
            Some("rustpy-0.2.0-linux-aarch64.tar.gz")
        );
        assert_eq!(pick(Linux, X86_64, Flatpak), None);
        assert_eq!(pick(Linux, X86_64, Snap), None);
        assert_eq!(
            pick(Windows, X86_64, Installer),
            Some("rustpy-0.2.0-windows-x86_64-setup.exe")
        );
        assert_eq!(
            pick(Windows, Aarch64, Installer),
            Some("rustpy-0.2.0-windows-arm64-setup.exe")
        );
        assert_eq!(
            pick(Windows, X86_64, Portable),
            Some("rustpy-0.2.0-windows-x86_64-portable.zip")
        );
        assert_eq!(
            pick(Windows, Aarch64, Portable),
            Some("rustpy-0.2.0-windows-arm64-portable.zip")
        );
        assert_eq!(
            pick(MacOs, Aarch64, AppBundle),
            Some("RustPy-0.2.0-universal.dmg")
        );
        assert_eq!(
            pick(MacOs, X86_64, Archive),
            Some("RustPy-0.2.0-universal.dmg")
        );
        assert_eq!(pick(Linux, Arch::Other, Archive), None);
        assert_eq!(pick(Os::Other, X86_64, Archive), None);
        // Older releases: an arch-specific .dmg, or only the cross-built Windows zip.
        let old = with_assets(&[
            "RustPy-0.1.0-arm64.dmg",
            "rustpy-windows-x86_64.zip",
            "rustpy-linux-x86_64.tar.gz",
        ]);
        let pick = |os, arch, install| {
            asset_for(&old, &Platform { os, arch, install }).map(|a| a.name.as_str())
        };
        assert_eq!(
            pick(MacOs, Aarch64, AppBundle),
            Some("RustPy-0.1.0-arm64.dmg")
        );
        assert_eq!(pick(MacOs, X86_64, AppBundle), None);
        assert_eq!(
            pick(Windows, X86_64, Installer),
            Some("rustpy-windows-x86_64.zip")
        );
        assert_eq!(pick(Linux, X86_64, AppImage), None);
        assert_eq!(pick(Linux, X86_64, Deb), None);
        assert_eq!(
            pick(Linux, X86_64, Archive),
            Some("rustpy-linux-x86_64.tar.gz")
        );
    }

    #[test]
    fn current_platform_matches_the_build() {
        let p = Platform::current();
        if cfg!(target_os = "linux") {
            assert_eq!(p.os, Os::Linux);
        }
        if cfg!(target_arch = "x86_64") {
            assert_eq!(p.arch, Arch::X86_64);
        }
    }

    #[test]
    fn install_kind_markers_and_ids_round_trip() {
        assert_eq!(InstallKind::from_marker("msi\r\n"), Some(InstallKind::Msi));
        assert_eq!(InstallKind::from_marker(" MSIX "), Some(InstallKind::Store));
        assert_eq!(InstallKind::from_marker("store"), Some(InstallKind::Store));
        assert_eq!(InstallKind::from_marker("portable"), None);
        assert_eq!(
            InstallKind::from_marker(InstallKind::Store.id()),
            Some(InstallKind::Store)
        );
        assert_eq!(
            InstallKind::from_marker(InstallKind::Msi.id()),
            Some(InstallKind::Msi)
        );
    }

    #[test]
    fn windows_install_kind_prefers_package_identity_then_marker() {
        // Package identity wins over everything beside the executable.
        assert_eq!(windows_install_kind(true, None, false), InstallKind::Store);
        assert_eq!(
            windows_install_kind(true, Some("msi"), true),
            InstallKind::Store
        );
        // Without identity the marker decides; an unknown or empty marker is the MSI's.
        assert_eq!(
            windows_install_kind(false, Some("msix"), false),
            InstallKind::Store
        );
        assert_eq!(
            windows_install_kind(false, Some("msi\r\n"), false),
            InstallKind::Msi
        );
        assert_eq!(
            windows_install_kind(false, Some(""), true),
            InstallKind::Msi
        );
        assert_eq!(
            windows_install_kind(false, None, true),
            InstallKind::Installer
        );
        assert_eq!(
            windows_install_kind(false, None, false),
            InstallKind::Portable
        );
    }

    #[test]
    fn store_installs_never_self_update_or_download() {
        assert!(!InstallKind::Store.self_updates());
        assert!(InstallKind::Store.updates_managed_externally());
        assert!(!InstallKind::Flatpak.updates_managed_externally());
        assert!(InstallKind::Msi.self_updates() && InstallKind::Portable.self_updates());
        let r = with_assets(&[
            "rustpy-0.2.0-windows-x64.msi",
            "rustpy-0.2.0-windows-x86_64-setup.exe",
            "rustpy-0.2.0-windows-x86_64-portable.zip",
        ]);
        let store = Platform {
            os: Os::Windows,
            arch: Arch::X86_64,
            install: InstallKind::Store,
        };
        assert!(asset_for(&r, &store).is_none());
        let msi = Platform {
            install: InstallKind::Msi,
            ..store
        };
        assert!(asset_for(&r, &msi).is_some());
    }

    #[test]
    fn no_package_identity_off_windows() {
        if !cfg!(windows) {
            assert!(!has_package_identity());
        }
    }
}

//! The update check against a recorded GitHub Releases API answer
//! (`GET /repos/themuddfamily/rustpy/releases`, trimmed).

use rustpy_updates::{
    asset_for, newest, parse_releases, update_for, Arch, Channel, InstallKind, Os, Platform,
    Version,
};

const FIXTURE: &str = include_str!("fixtures/github-releases.json");

#[test]
fn parses_the_recorded_release_list() {
    let releases = parse_releases(FIXTURE).unwrap();
    // The "nightly" tag is not a version and is left out.
    assert_eq!(
        releases.iter().map(|r| r.tag.as_str()).collect::<Vec<_>>(),
        ["v0.3.0-eap.1", "v0.2.0", "v0.1.1"]
    );
    let r = &releases[1];
    assert_eq!(r.name, "RustPy 0.2.0");
    assert_eq!(r.version.to_string(), "0.2.0");
    assert_eq!(
        r.page_url,
        "https://github.com/themuddfamily/rustpy/releases/tag/v0.2.0"
    );
    assert_eq!(r.date(), "2026-10-01");
    assert!(r
        .notes
        .starts_with("### Added\r\n- **Help › Check for Updates…**"));
    assert_eq!(r.assets.len(), 14);
    assert!(!r.is_early_access());
    assert!(releases[0].is_early_access());
    // An empty title falls back to the tag.
    assert_eq!(releases[2].name, "v0.1.1");
}

#[test]
fn filters_by_channel_and_offers_newer_releases() {
    let releases = parse_releases(FIXTURE).unwrap();
    assert_eq!(
        newest(&releases, Channel::Stable).map(|r| r.tag.as_str()),
        Some("v0.2.0")
    );
    assert_eq!(
        newest(&releases, Channel::Eap).map(|r| r.tag.as_str()),
        Some("v0.3.0-eap.1")
    );
    let current = Version::parse("0.1.1").unwrap();
    assert_eq!(
        update_for(&current, &releases, Channel::Stable, &[]).map(|r| r.tag.as_str()),
        Some("v0.2.0")
    );
    assert_eq!(
        update_for(&current, &releases, Channel::Eap, &[]).map(|r| r.tag.as_str()),
        Some("v0.3.0-eap.1")
    );
    assert!(update_for(
        &Version::parse("0.2.0").unwrap(),
        &releases,
        Channel::Stable,
        &[]
    )
    .is_none());
    assert!(update_for(&current, &releases, Channel::Stable, &["0.2.0".to_string()]).is_none());
    // The EAP build is offered to 0.2.0 users on the EAP channel only.
    let stable = Version::parse("0.2.0").unwrap();
    assert!(update_for(&stable, &releases, Channel::Stable, &[]).is_none());
    assert_eq!(
        update_for(&stable, &releases, Channel::Eap, &[]).map(|r| r.tag.as_str()),
        Some("v0.3.0-eap.1")
    );
}

#[test]
fn picks_downloads_from_the_recorded_release() {
    let releases = parse_releases(FIXTURE).unwrap();
    let r = releases.iter().find(|r| r.tag == "v0.2.0").unwrap();
    let url = |os, arch, install| {
        asset_for(r, &Platform { os, arch, install })
            .map(|a| a.url.rsplit('/').next().unwrap_or_default().to_string())
    };
    assert_eq!(
        url(Os::Linux, Arch::X86_64, InstallKind::Deb).as_deref(),
        Some("rustpy_0.2.0-1_amd64.deb")
    );
    assert_eq!(
        url(Os::Linux, Arch::Aarch64, InstallKind::Rpm).as_deref(),
        Some("rustpy-0.2.0-1.aarch64.rpm")
    );
    assert_eq!(
        url(Os::Linux, Arch::X86_64, InstallKind::AppImage).as_deref(),
        Some("RustPy-0.2.0-x86_64.AppImage")
    );
    assert_eq!(
        url(Os::Linux, Arch::Aarch64, InstallKind::Archive).as_deref(),
        Some("rustpy-0.2.0-linux-aarch64.tar.gz")
    );
    assert_eq!(url(Os::Linux, Arch::X86_64, InstallKind::Flatpak), None);
    assert_eq!(
        url(Os::Windows, Arch::Aarch64, InstallKind::Installer).as_deref(),
        Some("rustpy-0.2.0-windows-arm64-setup.exe")
    );
    assert_eq!(
        url(Os::Windows, Arch::X86_64, InstallKind::Portable).as_deref(),
        Some("rustpy-0.2.0-windows-x86_64-portable.zip")
    );
    assert_eq!(
        url(Os::MacOs, Arch::X86_64, InstallKind::AppBundle).as_deref(),
        Some("RustPy-0.2.0-universal.dmg")
    );
    // The older release only has bare archives.
    let old = releases.iter().find(|r| r.tag == "v0.1.1").unwrap();
    let old_url =
        |os, arch, install| asset_for(old, &Platform { os, arch, install }).map(|a| a.name.clone());
    assert_eq!(
        old_url(Os::MacOs, Arch::Aarch64, InstallKind::Archive).as_deref(),
        Some("rustpy-macos-aarch64.tar.gz")
    );
    assert_eq!(
        old_url(Os::Windows, Arch::X86_64, InstallKind::Installer).as_deref(),
        Some("rustpy-windows-x86_64.zip")
    );
}

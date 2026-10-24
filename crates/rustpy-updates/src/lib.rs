//! RustPy IDE: update checks (roadmap §8: "Check for Updates", Stable / EAP channels,
//! release notes and "What's New").
//!
//! The IDE reads the release list of the configured GitHub repository from the Releases
//! API ([`fetch_releases`]), keeps the releases of the chosen [`Channel`], offers the newest
//! one when it is newer than the running [`Version`] and not ignored ([`update_for`]), and
//! picks the download for this platform ([`asset_for`]). The automatic check runs at most
//! once a day ([`schedule`]). What's New shows the CHANGELOG.md section of the running
//! version ([`changelog`]). Everything but [`fetch_releases`] is pure and unit-tested.

pub mod changelog;
pub mod fetch;
pub mod install;
pub mod release;
pub mod schedule;
pub mod version;

pub use fetch::{
    fetch_from_test_feed, fetch_releases, releases_api_url, releases_page_url, FetchError,
    DEFAULT_REPOSITORY,
};
pub use release::{
    asset_for, has_package_identity, newest, parse_releases, update_for, windows_install_kind,
    Arch, Asset, Channel, InstallKind, Os, Platform, Release, STORE_UPDATES_MESSAGE,
};
pub use schedule::{check_due, Scheduler};
pub use version::Version;

/// The version of this build (the workspace version in the root `Cargo.toml`).
pub const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

/// [`CURRENT_VERSION`], parsed.
pub fn current_version() -> Version {
    Version::parse(CURRENT_VERSION).unwrap_or(Version {
        major: 0,
        minor: 0,
        patch: 0,
        pre: Vec::new(),
        build: String::new(),
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_build_version_parses() {
        assert!(crate::Version::parse(crate::CURRENT_VERSION).is_some());
        assert_eq!(crate::current_version().to_string(), crate::CURRENT_VERSION);
    }
}

//! Release acceptance for the self-update path against a real HTTPS release feed.
//!
//! Ignored by default: it needs a served feed and an installed copy to replace. Serve a
//! GitHub-style Releases API (`<base>/api/v3/repos/<owner>/<name>/releases`) whose release
//! carries a Linux archive and `SHA256SUMS`, trust its CA through `SSL_CERT_FILE`, then run
//!
//! ```sh
//! RUSTPY_ACCEPTANCE_UPDATE_REPOSITORY=https://127.0.0.1:18443/acme/rustpy \
//! RUSTPY_ACCEPTANCE_UPDATE_TARGET=/path/to/installed/rustpy \
//! RUSTPY_ACCEPTANCE_UPDATE_STAGE=/path/to/staging-dir \
//! SSL_CERT_FILE=ca.pem cargo test -p rustpy-updates --test local_feed_install -- --ignored
//! ```
//!
//! The test fetches the release list over the network, picks this platform's archive,
//! downloads and verifies it, and starts the detached install helper aimed at
//! `RUSTPY_ACCEPTANCE_UPDATE_TARGET`. The helper waits for the test process to exit,
//! replaces the target and runs `<target> --version`; its output lands in
//! `<staging-dir>/update-*/install.log` until the helper removes the staging folder.
//! Check the replaced binary afterwards.

use rustpy_updates::install::{prepare, UpdateOwner};
use rustpy_updates::{fetch_releases, releases_api_url, update_for, Channel, Platform, Version};

#[test]
#[ignore = "needs a served HTTPS release feed; see the module documentation"]
fn downloads_verifies_and_stages_an_archive_update() {
    let var = |name: &str| std::env::var(name).unwrap_or_else(|_| panic!("set {name}"));
    let repository = var("RUSTPY_ACCEPTANCE_UPDATE_REPOSITORY");
    let target = std::path::PathBuf::from(var("RUSTPY_ACCEPTANCE_UPDATE_TARGET"));
    let stage = std::path::PathBuf::from(var("RUSTPY_ACCEPTANCE_UPDATE_STAGE"));
    assert!(
        target.is_file(),
        "{} is not an installed binary",
        target.display()
    );

    let releases = fetch_releases(&releases_api_url(&repository).unwrap()).unwrap();
    let current = Version::parse("0.0.1").unwrap();
    let release = update_for(&current, &releases, Channel::Stable, &[]).expect("no update");
    let platform = Platform {
        install: rustpy_updates::InstallKind::Archive,
        ..Platform::current()
    };
    let owner = UpdateOwner::default();
    let mut prepared = prepare(release, &platform, &stage, &owner).unwrap();
    assert!(prepared.artifact.is_file());
    assert_eq!(prepared.sha256.len(), 64);
    prepared.executable = target;
    prepared.arguments = vec!["--version".into()];
    prepared.launch_after_exit().unwrap();
}

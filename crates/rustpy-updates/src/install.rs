//! Owned, checksum-verified update staging. Installation starts only after the host
//! accepts its normal save/process quit transaction.
use crate::{Asset, InstallKind, Platform, Release};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    },
    time::Duration,
};

static NEXT: AtomicU64 = AtomicU64::new(1);
#[derive(Clone, Debug)]
pub struct UpdateOwner {
    pub id: u64,
    cancelled: Arc<AtomicBool>,
    bytes: Arc<AtomicU64>,
}
impl Default for UpdateOwner {
    fn default() -> Self {
        Self {
            id: NEXT.fetch_add(1, Ordering::Relaxed),
            cancelled: Arc::new(AtomicBool::new(false)),
            bytes: Arc::new(AtomicU64::new(0)),
        }
    }
}
impl UpdateOwner {
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }
    pub fn downloaded(&self) -> u64 {
        self.bytes.load(Ordering::Acquire)
    }
    pub fn stamp(&self) -> String {
        format!("{}:{}", self.id, self.cancelled.load(Ordering::Acquire))
    }
    fn check(&self) -> Result<(), String> {
        if self.cancelled.load(Ordering::Acquire) {
            Err("Update cancelled".into())
        } else {
            Ok(())
        }
    }
}
#[derive(Clone, Debug)]
pub struct PreparedUpdate {
    pub version: String,
    pub artifact: PathBuf,
    pub executable: PathBuf,
    pub install: InstallKind,
    pub arguments: Vec<String>,
    pub binary: bool,
    pub sha256: String,
}
fn https(value: &str) -> Result<(), String> {
    let url = url::Url::parse(value).map_err(|e| e.to_string())?;
    if url.scheme() != "https" || !url.username().is_empty() || url.password().is_some() {
        return Err("Updates require an HTTPS release asset without embedded credentials".into());
    }
    Ok(())
}
fn download(
    asset: &Asset,
    destination: &Path,
    limit: u64,
    owner: &UpdateOwner,
) -> Result<(), String> {
    https(&asset.url)?;
    owner.check()?;
    if asset.size > limit {
        return Err("Update asset exceeds the size limit".into());
    }
    // Follow only HTTPS redirects; never carry credentials to another origin.
    let mut address = asset.url.clone();
    let response = loop_response(&mut address)?;
    let mut input = response.into_reader();
    let mut output = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(destination)
        .map_err(|e| e.to_string())?;
    let mut total = 0u64;
    let mut buffer = [0; 65536];
    let started = std::time::Instant::now();
    loop {
        owner.check()?;
        if started.elapsed() > Duration::from_secs(900) {
            return Err("Update download timed out".into());
        }
        let count = input.read(&mut buffer).map_err(|e| e.to_string())?;
        if count == 0 {
            break;
        }
        total += count as u64;
        if total > limit {
            return Err("Update download exceeds the size limit".into());
        }
        output
            .write_all(&buffer[..count])
            .map_err(|e| e.to_string())?;
        owner.bytes.fetch_add(count as u64, Ordering::Release);
    }
    output.sync_all().map_err(|e| e.to_string())?;
    if asset.size != 0 && total != asset.size {
        return Err("Incomplete update download".into());
    }
    Ok(())
}
fn loop_response(address: &mut String) -> Result<ureq::Response, String> {
    for _ in 0..6 {
        https(address)?;
        let response = rustpy_net::agent_builder(address)
            .timeout_connect(Duration::from_secs(15))
            .timeout_read(Duration::from_secs(5))
            .redirects(0)
            .build()
            .get(address)
            .set("User-Agent", rustpy_net::USER_AGENT)
            .call()
            .map_err(|e| e.to_string())?;
        if (300..400).contains(&response.status()) {
            let location = response
                .header("Location")
                .ok_or("Missing update redirect")?;
            *address = url::Url::parse(address)
                .map_err(|e| e.to_string())?
                .join(location)
                .map_err(|e| e.to_string())?
                .to_string();
            continue;
        }
        return Ok(response);
    }
    Err("Too many update redirects".into())
}
pub fn checksum_for(sums: &str, name: &str) -> Result<String, String> {
    let mut found = None;
    for line in sums.lines() {
        let Some((digest, file)) = line.split_once(char::is_whitespace) else {
            continue;
        };
        if file.trim().trim_start_matches('*') != name {
            continue;
        }
        if digest.len() != 64 || !digest.bytes().all(|b| b.is_ascii_hexdigit()) || found.is_some() {
            return Err("Invalid or duplicate release checksum".into());
        }
        found = Some(digest.to_ascii_lowercase());
    }
    found.ok_or_else(|| format!("Release checksum is missing for {name}"))
}
fn digest(path: &Path) -> Result<String, String> {
    let mut input = fs::File::open(path).map_err(|e| e.to_string())?;
    let mut hash = Sha256::new();
    let mut buffer = [0; 65536];
    loop {
        let count = input.read(&mut buffer).map_err(|e| e.to_string())?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    Ok(format!("{:x}", hash.finalize()))
}
/// Download a platform artifact and its release checksums. No package is executed here.
pub fn prepare(
    release: &Release,
    platform: &Platform,
    directory: &Path,
    owner: &UpdateOwner,
) -> Result<PreparedUpdate, String> {
    if platform.install.updates_managed_externally() {
        return Err(crate::STORE_UPDATES_MESSAGE.into());
    }
    if platform.os == crate::Os::MacOs && platform.install != InstallKind::AppBundle {
        return Err("Install the macOS app bundle from the release page before using automatic installation".into());
    }
    let asset = crate::asset_for(release, platform)
        .ok_or("Use the Flatpak/Snap package manager for this installation")?;
    if Path::new(&asset.name).file_name().and_then(|s| s.to_str()) != Some(asset.name.as_str())
        || asset.name.contains(['\\', '/', '\0'])
    {
        return Err("Invalid update artifact name".into());
    }
    let sums = release
        .assets
        .iter()
        .find(|a| a.name == "SHA256SUMS")
        .ok_or("This release has no SHA256SUMS; use its download page")?;
    fs::create_dir_all(directory).map_err(|e| e.to_string())?;
    let stage = directory.join(format!("update-{}-{}", std::process::id(), owner.id));
    fs::create_dir(&stage).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&stage, fs::Permissions::from_mode(0o700))
            .map_err(|e| e.to_string())?;
    }
    let result = (|| {
        let checksums = stage.join("SHA256SUMS");
        download(sums, &checksums, 4 * 1024 * 1024, owner)?;
        let checksums = fs::read_to_string(&checksums).map_err(|e| e.to_string())?;
        let executable = std::env::var_os("APPIMAGE")
            .map(PathBuf::from)
            .map(Ok)
            .unwrap_or_else(std::env::current_exe)
            .map_err(|e| e.to_string())?;
        if let Some(prepared) =
            prepare_patch(release, platform, &stage, &executable, &checksums, owner)?
        {
            return Ok(prepared);
        }
        let expected = checksum_for(&checksums, &asset.name)?;
        let artifact = stage.join(&asset.name);
        download(asset, &artifact, 2 * 1024 * 1024 * 1024, owner)?;
        if digest(&artifact)? != expected {
            return Err("The update checksum does not match; nothing was installed".into());
        }
        owner.check()?;
        Ok(PreparedUpdate {
            version: release.version.to_string(),
            artifact,
            executable,
            install: platform.install,
            arguments: std::env::args().skip(1).collect(),
            binary: false,
            sha256: expected,
        })
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(&stage);
    }
    result
}
#[cfg(any(not(windows), test))]
fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}
#[cfg(any(windows, test))]
fn windows_argument(value: &str) -> String {
    let mut result = String::from("\"");
    let mut slashes = 0;
    for ch in value.chars() {
        if ch == '\\' {
            slashes += 1;
            continue;
        }
        if ch == '"' {
            result.push_str(&"\\".repeat(slashes * 2 + 1));
        } else {
            result.push_str(&"\\".repeat(slashes));
        }
        slashes = 0;
        result.push(ch);
    }
    result.push_str(&"\\".repeat(slashes * 2));
    result.push('"');
    result
}
impl PreparedUpdate {
    /// Spawn a detached helper that waits for this process before replacing/installing.
    /// The caller must have already committed its coordinated quit transaction.
    pub fn launch_after_exit(&self) -> Result<(), String> {
        // A packaged (MSIX) install lives in the read-only WindowsApps folder.
        if self.install.updates_managed_externally() {
            return Err(crate::STORE_UPDATES_MESSAGE.into());
        }
        if self.sha256.len() != 64
            || !self.sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
            || !self.artifact.is_file()
        {
            return Err("The staged update or its checksum is unavailable".into());
        }
        let script = self.artifact.parent().ok_or("Missing staging directory")?;
        #[cfg(windows)]
        {
            let ps = script.join("install.ps1");
            let q = |p: &Path| format!("'{}'", p.to_string_lossy().replace('\'', "''"));
            let args = self
                .arguments
                .iter()
                .map(|a| windows_argument(a))
                .collect::<Vec<_>>()
                .join(" ");
            let restart = format!("$start=New-Object Diagnostics.ProcessStartInfo; $start.FileName={}; $start.Arguments='{}'; $start.UseShellExecute=$true; [Diagnostics.Process]::Start($start)|Out-Null", q(&self.executable), args.replace('\'', "''"));
            let command = if self.artifact.extension().is_some_and(|x| x == "msi") {
                format!(
                    "$result=Start-Process msiexec.exe -ArgumentList @('/i','\"{}\"','/passive','/norestart') -Wait -PassThru; if($result.ExitCode -notin @(0,3010)){{throw 'MSI installation failed'}}; {restart}",
                    self.artifact.to_string_lossy().replace('\'', "''")
                )
            } else if self.artifact.extension().is_some_and(|x| x == "zip") {
                format!("Add-Type -AssemblyName System.IO.Compression.FileSystem; $zip=[IO.Compression.ZipFile]::OpenRead({}); try {{$entry=$zip.GetEntry('rustpy.exe'); if(-not $entry -or $entry.Length -gt 1073741824){{throw 'Invalid portable update'}}; [IO.Compression.ZipFileExtensions]::ExtractToFile($entry,{},$false)}} finally {{$zip.Dispose()}}; [IO.File]::Replace({},{},{},$true); {restart}",q(&self.artifact),q(&self.executable.with_extension("exe.update")),q(&self.executable.with_extension("exe.update")),q(&self.executable),q(&self.executable.with_extension("exe.previous")))
            } else {
                format!("$result=Start-Process -FilePath {} -ArgumentList @('/SILENT','/CLOSEAPPLICATIONS','/NORESTART') -Wait -PassThru; if($result.ExitCode -ne 0){{throw 'Installer failed'}}; {restart}", q(&self.artifact))
            };
            fs::write(&ps, format!("$ErrorActionPreference='Stop'\nStart-Transcript -Path (Join-Path $PSScriptRoot 'install.log')\nWait-Process -Id {} -ErrorAction SilentlyContinue\nif ((Get-FileHash -LiteralPath {} -Algorithm SHA256).Hash.ToLower() -ne '{}') {{throw 'Staged update checksum changed'}}\n{command}\nStop-Transcript\nRemove-Item -LiteralPath $PSScriptRoot -Recurse -Force\n", std::process::id(),q(&self.artifact),self.sha256.to_ascii_lowercase())).map_err(|e| e.to_string())?;
            use std::os::windows::process::CommandExt;
            std::process::Command::new("powershell.exe")
                .args([
                    "-NoProfile",
                    "-NonInteractive",
                    "-ExecutionPolicy",
                    "Bypass",
                    "-File",
                ])
                .arg(ps)
                .creation_flags(0x08000000 | 0x00000008)
                .spawn()
                .map_err(|e| e.to_string())?;
        }
        #[cfg(not(windows))]
        {
            let target = quote(&self.executable.to_string_lossy());
            let artifact = quote(&self.artifact.to_string_lossy());
            let args = self
                .arguments
                .iter()
                .map(|a| quote(a))
                .collect::<Vec<_>>()
                .join(" ");
            let command = if self.binary {
                format!("chmod 755 {artifact}\ncp {artifact} {target}.update\nmv -f {target}.update {target}\nexec {target} {args}")
            } else {
                match self.install {
                InstallKind::AppImage => format!("chmod 755 {artifact}\ncp {artifact} {target}.update\nmv -f {target}.update {target}\nexec {target} {args}"),
                InstallKind::Deb => format!("pkexec dpkg -i {artifact}\nexec {target} {args}"),
                InstallKind::Rpm => format!("pkexec rpm -U {artifact}\nexec {target} {args}"),
                InstallKind::AppBundle => format!("mount=\"$stage/mount\"; mkdir -p \"$mount\"\nhdiutil attach -nobrowse -readonly -mountpoint \"$mount\" {artifact}\ntrap 'hdiutil detach \"$mount\"' EXIT\ncodesign --verify --deep --strict \"$mount/RustPy.app\"\nspctl --assess --type execute \"$mount/RustPy.app\"\napp={target}; app=\"${{app%/Contents/MacOS/rustpy}}\"\nditto \"$mount/RustPy.app\" \"$app.update\"\nmv \"$app\" \"$app.previous\"\nif mv \"$app.update\" \"$app\"; then rm -rf \"$app.previous\"; else mv \"$app.previous\" \"$app\"; exit 1; fi\nhdiutil detach \"$mount\"\ntrap - EXIT\nopen -a \"$app\" --args {args}"),
                InstallKind::Flatpak | InstallKind::Snap => return Err("Use the installation's package manager".into()),
                _ => format!("tar -xOzf {artifact} rustpy > {target}.update\nchmod 755 {target}.update\nmv -f {target}.update {target}\nexec {target} {args}"),
            }
            };
            let command = command.replace("\nexec ", "\nrm -rf \"$stage\"\nexec ");
            let path = script.join("install.sh");
            let checksum = if cfg!(target_os = "macos") {
                "shasum -a 256"
            } else {
                "sha256sum"
            };
            fs::write(&path, format!("#!/bin/sh\nset -eu\nstage={}\nremaining=600\nwhile kill -0 {} 2>/dev/null; do remaining=$((remaining-1)); if [ \"$remaining\" -eq 0 ]; then echo 'Parent did not exit; update retained' >&2; exit 1; fi; sleep 1; done\nactual=$({checksum} {artifact}); actual=${{actual%% *}}\nif [ \"$actual\" != '{}' ]; then echo 'Staged update checksum changed' >&2; exit 1; fi\n{command}\n", quote(&script.to_string_lossy()),std::process::id(),self.sha256.to_ascii_lowercase())).map_err(|e| e.to_string())?;
            use std::os::unix::process::CommandExt;
            let mut helper = std::process::Command::new("sh");
            helper.process_group(0);
            helper
                .arg(path)
                .stdin(std::process::Stdio::null())
                .stdout(fs::File::create(script.join("install.log")).map_err(|e| e.to_string())?)
                .stderr(
                    fs::OpenOptions::new()
                        .append(true)
                        .open(script.join("install.log"))
                        .map_err(|e| e.to_string())?,
                )
                .spawn()
                .map_err(|e| e.to_string())?;
        }
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn transfer_progress_does_not_change_quit_consent_identity() {
        let owner = UpdateOwner::default();
        let stamp = owner.stamp();
        owner.bytes.fetch_add(8192, Ordering::Release);
        assert_eq!(owner.stamp(), stamp);
        owner.cancel();
        assert_ne!(owner.stamp(), stamp);
    }
    #[test]
    fn checksums_require_exact_names_and_unique_valid_hashes() {
        let hash = "a".repeat(64);
        assert_eq!(
            checksum_for(&format!("{hash}  rustpy.tar.gz\n"), "rustpy.tar.gz"),
            Ok(hash.clone())
        );
        assert!(checksum_for(&format!("{hash}  ../rustpy.tar.gz"), "rustpy.tar.gz").is_err());
        assert!(checksum_for(&format!("{hash}  a\n{hash}  a"), "a").is_err());
    }
    #[test]
    fn staging_cancellation_and_https_are_explicit() {
        let owner = UpdateOwner::default();
        owner.cancel();
        assert!(owner.check().is_err());
        assert!(https("http://example.com/a").is_err());
        assert!(https("https://user:secret@example.com/a").is_err());
        assert_eq!(quote("a'b"), "'a'\\''b'");
        assert_eq!(
            windows_argument("C:\\project folder\\"),
            "\"C:\\project folder\\\\\""
        );
        assert_eq!(windows_argument("a\"b"), "\"a\\\"b\"");
    }
    #[test]
    fn store_installs_refuse_to_stage_or_install_updates() {
        let release = crate::parse_releases(r#"[{"tag_name": "v9.0.0", "assets": [{"name": "rustpy-9.0.0-windows-x64.msi", "browser_download_url": "https://example.com/a.msi", "size": 1}, {"name": "SHA256SUMS", "browser_download_url": "https://example.com/SHA256SUMS", "size": 1}]}]"#).unwrap().remove(0);
        let platform = Platform {
            os: crate::Os::Windows,
            arch: crate::Arch::X86_64,
            install: InstallKind::Store,
        };
        let directory =
            std::env::temp_dir().join(format!("rustpy-store-update-{}", std::process::id()));
        let refused = prepare(&release, &platform, &directory, &UpdateOwner::default());
        assert_eq!(refused.unwrap_err(), crate::STORE_UPDATES_MESSAGE);
        assert!(!directory.exists(), "nothing is staged for a Store install");
        let prepared = PreparedUpdate {
            version: "9.0.0".into(),
            artifact: directory.join("a.msi"),
            executable: directory.join("rustpy.exe"),
            install: InstallKind::Store,
            arguments: Vec::new(),
            binary: false,
            sha256: "a".repeat(64),
        };
        assert_eq!(
            prepared.launch_after_exit().unwrap_err(),
            crate::STORE_UPDATES_MESSAGE
        );
    }
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct PatchManifest {
    from: String,
    to: String,
    artifact: String,
    old_sha256: String,
    new_sha256: String,
}
fn prepare_patch(
    release: &Release,
    platform: &Platform,
    stage: &Path,
    executable: &Path,
    sums: &str,
    owner: &UpdateOwner,
) -> Result<Option<PreparedUpdate>, String> {
    // Patches operate on one binary only. Bundles, installers and package-manager
    // installations always use their complete signed/native packages.
    if cfg!(windows)
        || !matches!(
            platform.install,
            InstallKind::Archive | InstallKind::AppImage
        )
    {
        return Ok(None);
    }
    let full = crate::asset_for(release, platform).ok_or("Missing platform artifact")?;
    let name = format!("{}.from-{}.patch.json", full.name, crate::CURRENT_VERSION);
    let Some(manifest_asset) = release.assets.iter().find(|a| a.name == name) else {
        return Ok(None);
    };
    let path = stage.join("patch.json");
    download(manifest_asset, &path, 65536, owner)?;
    if digest(&path)? != checksum_for(sums, &name)? {
        return Err("Patch manifest checksum mismatch".into());
    }
    let manifest: PatchManifest =
        serde_json::from_slice(&fs::read(&path).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
    if manifest.from != crate::CURRENT_VERSION
        || manifest.to != release.version.to_string()
        || manifest.old_sha256 != digest(executable)?
    {
        return Ok(None);
    }
    if manifest.new_sha256.len() != 64
        || !manifest.new_sha256.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return Err("Invalid patched binary checksum".into());
    }
    let asset = release
        .assets
        .iter()
        .find(|a| a.name == manifest.artifact)
        .ok_or("Patch asset is missing")?;
    let patch = stage.join("binary.bsdiff");
    download(asset, &patch, 512 * 1024 * 1024, owner)?;
    if digest(&patch)? != checksum_for(sums, &asset.name)? {
        return Err("Patch checksum mismatch".into());
    }
    let output = stage.join("patched-rustpy");
    let mut child = match std::process::Command::new("bspatch")
        .arg(executable)
        .arg(&output)
        .arg(&patch)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(child) => child,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.to_string()),
    };
    let start = std::time::Instant::now();
    loop {
        if owner.check().is_err()
            || start.elapsed() > Duration::from_secs(120)
            || fs::metadata(&output).is_ok_and(|m| m.len() > 2 * 1024 * 1024 * 1024)
        {
            let _ = child.kill();
            let _ = child.wait();
            return Err("Patch preparation cancelled or timed out".into());
        }
        let status = match child.try_wait() {
            Ok(status) => status,
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error.to_string());
            }
        };
        match status {
            Some(status) if status.success() => break,
            Some(_) => {
                let _ = fs::remove_file(&output);
                return Ok(None);
            }
            None => std::thread::sleep(Duration::from_millis(50)),
        }
    }
    if digest(&output)? != manifest.new_sha256 {
        return Err("Patched binary checksum mismatch".into());
    }
    Ok(Some(PreparedUpdate {
        version: manifest.to,
        artifact: output,
        executable: executable.to_owned(),
        install: platform.install,
        arguments: std::env::args().skip(1).collect(),
        binary: true,
        sha256: manifest.new_sha256,
    }))
}

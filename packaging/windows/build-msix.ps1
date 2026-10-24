<#
.SYNOPSIS
    Builds RustPy MSIX packages (Microsoft Store / sideloading) on Windows with makeappx.exe
    from the Windows 10/11 SDK, optionally an .msixbundle of several architectures.

.DESCRIPTION
    powershell -ExecutionPolicy Bypass -File packaging\windows\build-msix.ps1 -Target x86_64-pc-windows-msvc -NoBuild

    The package identity comes from packaging\windows\msix\identity.env (placeholders until
    the Partner Center account exists), overridable with the RUSTPY_MSIX_IDENTITY_NAME,
    RUSTPY_MSIX_PUBLISHER, RUSTPY_MSIX_PUBLISHER_DISPLAY_NAME and RUSTPY_MSIX_DISPLAY_NAME
    environment variables. Store submissions are uploaded unsigned: the Store signs them.
    -TestSign signs a copy with a throw-away self-signed certificate (subject = Publisher)
    for local sideloading and exports it as rustpy-msix-test.cer.

    Outputs in <target dir>\msix (CARGO_TARGET_DIR is honoured):
        rustpy-<version>-windows-<arch>.msix
        rustpy-<version>-windows.msixbundle            (-Bundle)
        rustpy-<version>-windows-<arch>-test-signed.msix, rustpy-msix-test.cer   (-TestSign)

.PARAMETER Target
    One or more Rust target triples (x86_64-pc-windows-msvc, aarch64-pc-windows-msvc).
    Without -Target, -Bundle bundles the .msix files already in the output directory
    (release.yml builds each architecture on its own runner, then bundles them).
.PARAMETER NoBuild
    Package the existing release binaries instead of running cargo build.
.PARAMETER Bundle
    Also write an .msixbundle of every package in the output directory for this version.
.PARAMETER TestSign
    Also write test-signed copies (signtool.exe from the same SDK).
.PARAMETER OutDir
    Output directory (default: <target dir>\msix).
#>
[CmdletBinding()]
param(
    [string[]]$Target = @(),
    [string]$OutDir = "",
    [switch]$NoBuild,
    [switch]$Bundle,
    [switch]$TestSign
)
$ErrorActionPreference = "Stop"
if (-not $Target.Count -and -not $Bundle) { throw "Pass -Target (and/or -Bundle to bundle the .msix files already in -OutDir)" }
$Root = (Resolve-Path (Join-Path $PSScriptRoot "..\..")).Path
& python (Join-Path $Root "packaging/update-notices.py") --check
if ($LASTEXITCODE) { throw "Bundled license notices are missing or stale" }
$Msix = Join-Path $PSScriptRoot "msix"
$TargetDir = if ($env:CARGO_TARGET_DIR) { [IO.Path]::GetFullPath($env:CARGO_TARGET_DIR) } else { Join-Path $Root "target" }
if (-not $OutDir) { $OutDir = Join-Path $TargetDir "msix" }
New-Item -ItemType Directory -Force $OutDir | Out-Null

# Workspace version, prerelease/build suffix dropped, four parts with the last one 0.
$inWorkspace = $false; $Version = $null
foreach ($l in Get-Content (Join-Path $Root "Cargo.toml")) {
    if ($l -match '^\[workspace\.package\]') { $inWorkspace = $true; continue }
    if ($l -match '^\[') { $inWorkspace = $false }
    if ($inWorkspace -and $l -match '^version = "([^"]+)"') { $Version = $Matches[1]; break }
}
if (-not $Version) { throw "No [workspace.package] version in Cargo.toml" }
$parts = @((($Version -split '[-+]')[0] -split '\.') + @('0', '0', '0'))[0..2]
$MsixVersion = "$($parts -join '.').0"

$Identity = @{}
foreach ($l in Get-Content (Join-Path $Msix "identity.env")) {
    if ($l -match '^\s*#' -or $l -notmatch '=') { continue }
    $k, $v = $l -split '=', 2
    $Identity[$k.Trim()] = $v
}
foreach ($k in 'IDENTITY_NAME', 'PUBLISHER', 'PUBLISHER_DISPLAY_NAME', 'DISPLAY_NAME') {
    $override = [Environment]::GetEnvironmentVariable("RUSTPY_MSIX_$k")
    if ($override) { $Identity[$k] = $override }
    if (-not $Identity[$k]) { throw "$k is empty" }
}
if (-not $Identity['PUBLISHER'].StartsWith('CN=')) { throw "PUBLISHER must start with CN=" }

function Find-SdkTool($name) {
    $cmd = Get-Command $name -ErrorAction SilentlyContinue
    if ($cmd) { return $cmd.Source }
    $hostArch = if ($env:PROCESSOR_ARCHITECTURE -eq 'ARM64') { 'arm64' } else { 'x64' }
    $kits = Join-Path ${env:ProgramFiles(x86)} "Windows Kits\10\bin"
    $found = Get-ChildItem $kits -Directory -ErrorAction SilentlyContinue | Where-Object Name -match '^10\.' |
        Sort-Object { [version]$_.Name } -Descending |
        ForEach-Object { Join-Path $_.FullName "$hostArch\$name" } | Where-Object { Test-Path $_ } | Select-Object -First 1
    if (-not $found) { throw "$name not found: install the Windows 10/11 SDK (App Certification Kit / MSIX tools)" }
    return $found
}
$MakeAppx = Find-SdkTool "makeappx.exe"
function Xml($s) { return [Security.SecurityElement]::Escape($s) }

$Packages = @()
Push-Location $Root
try {
    foreach ($t in $Target) {
        $Arch = switch ($t) { "x86_64-pc-windows-msvc" { "x64" } "aarch64-pc-windows-msvc" { "arm64" } default { throw "Unsupported MSIX target $t" } }
        if (-not $NoBuild) {
            & cargo build --locked --release -p rustpy --target $t --features native-web-preview
            if ($LASTEXITCODE) { throw "Cargo failed" }
        }
        $Exe = Join-Path $TargetDir "$t\release\rustpy.exe"
        if (-not (Test-Path $Exe)) { throw "$Exe not found" }
        $Stage = Join-Path $OutDir "stage-$Arch"
        if (Test-Path $Stage) { Remove-Item -Recurse -Force $Stage }
        New-Item -ItemType Directory -Force (Join-Path $Stage "Assets") | Out-Null
        Copy-Item $Exe (Join-Path $Stage "rustpy.exe")
        Copy-Item (Join-Path $Root "THIRD-PARTY-NOTICES.txt") $Stage
        Copy-Item (Join-Path $Root "LICENSE") $Stage
        & python (Join-Path $Root "packaging/stage-native-licenses.py") --binary (Join-Path $Stage "rustpy.exe") --runtime-root $Stage --output (Join-Path $Stage "Native-Licenses.zip")
        if ($LASTEXITCODE) { throw "Native license/source collection failed" }
        # Fallback install-kind marker; package identity (GetCurrentPackageFullName) wins.
        Set-Content -LiteralPath (Join-Path $Stage "rustpy-install.kind") -Value 'msix' -Encoding Ascii
        Copy-Item (Join-Path $Msix "Assets\*.png") (Join-Path $Stage "Assets")
        $Manifest = (Get-Content -Raw (Join-Path $Msix "AppxManifest.xml.in")).
            Replace('@IDENTITY_NAME@', (Xml $Identity['IDENTITY_NAME'])).
            Replace('@PUBLISHER_DISPLAY_NAME@', (Xml $Identity['PUBLISHER_DISPLAY_NAME'])).
            Replace('@PUBLISHER@', (Xml $Identity['PUBLISHER'])).
            Replace('@DISPLAY_NAME@', (Xml $Identity['DISPLAY_NAME'])).
            Replace('@VERSION@', $MsixVersion).Replace('@ARCH@', $Arch)
        if ($Manifest -match '@[A-Z_]+@') { throw "Unsubstituted placeholder in AppxManifest.xml" }
        [IO.File]::WriteAllText((Join-Path $Stage "AppxManifest.xml"), $Manifest, (New-Object Text.UTF8Encoding $false))
        $Package = Join-Path $OutDir "rustpy-$Version-windows-$Arch.msix"
        & $MakeAppx pack /o /h SHA256 /d $Stage /p $Package
        if ($LASTEXITCODE) { throw "makeappx pack failed" }
        $Packages += $Package
    }
} finally { Pop-Location }

if ($Bundle) {
    $BundleInput = Join-Path $OutDir "bundle-input"
    if (Test-Path $BundleInput) { Remove-Item -Recurse -Force $BundleInput }
    New-Item -ItemType Directory -Force $BundleInput | Out-Null
    Get-ChildItem $OutDir -Filter "rustpy-$Version-windows-*.msix" | Where-Object Name -notmatch 'test-signed' |
        ForEach-Object { Copy-Item $_.FullName $BundleInput }
    $BundlePath = Join-Path $OutDir "rustpy-$Version-windows.msixbundle"
    & $MakeAppx bundle /o /bv $MsixVersion /d $BundleInput /p $BundlePath
    if ($LASTEXITCODE) { throw "makeappx bundle failed" }
    Remove-Item -Recurse -Force $BundleInput
}

if ($TestSign) {
    $SignTool = Find-SdkTool "signtool.exe"
    # Throw-away certificate in the user's store only for the time of signing.
    $cert = New-SelfSignedCertificate -Type Custom -Subject $Identity['PUBLISHER'] -KeyUsage DigitalSignature `
        -FriendlyName "RustPy MSIX test signing" -CertStoreLocation "Cert:\CurrentUser\My" `
        -TextExtension @("2.5.29.37={text}1.3.6.1.5.5.7.3.3", "2.5.29.19={text}") -NotAfter (Get-Date).AddDays(30)
    try {
        Export-Certificate -Cert $cert -FilePath (Join-Path $OutDir "rustpy-msix-test.cer") | Out-Null
        foreach ($p in $Packages) {
            $Signed = $p -replace '\.msix$', '-test-signed.msix'
            Copy-Item $p $Signed -Force
            & $SignTool sign /fd SHA256 /sha1 $cert.Thumbprint /s My $Signed
            if ($LASTEXITCODE) { throw "signtool failed" }
        }
    } finally { Remove-Item -LiteralPath "Cert:\CurrentUser\My\$($cert.Thumbprint)" -ErrorAction SilentlyContinue }
}
Get-ChildItem $OutDir -File | Where-Object Extension -in '.msix', '.msixbundle', '.cer' | ForEach-Object { Write-Host "built $($_.FullName)" }

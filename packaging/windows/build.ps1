<#
.SYNOPSIS
    Builds the RustPy Windows release artifacts: a portable zip and, when Inno Setup 6 is
    installed, the installer compiled from packaging\windows\rustpy.iss.

.DESCRIPTION
    Run from any directory (the script finds the repository root itself):

        powershell -ExecutionPolicy Bypass -File packaging\windows\build.ps1

    Steps:
      1. cargo build --locked --release -p rustpy (skipped with -NoBuild)
      2. rustpy.ico rendered from packaging\linux\rustpy.svg when ImageMagick ("magick")
         is installed; otherwise the installer uses the committed packaging\windows\rustpy.ico
      3. the portable zip: rustpy.exe, rustpy-portable.cmd (keeps the settings in a
         "config" folder next to the executable) and README.txt
      4. the installer, when ISCC.exe is found: $env:ISCC, "Inno Setup 6" under
         Program Files, Program Files (x86) or %LOCALAPPDATA%\Programs, or PATH

    Outputs in <target dir>\windows (CARGO_TARGET_DIR is honoured):
        rustpy-<version>-windows-<arch>-portable.zip
        rustpy-<version>-windows-<arch>-setup.exe

.PARAMETER NoBuild
    Package the existing release binary instead of building it.
.PARAMETER Target
    Rust target triple, e.g. aarch64-pc-windows-msvc (default: the host toolchain).
.PARAMETER OutDir
    Output directory (default: <target dir>\windows).
.PARAMETER SkipZip
    Do not create the portable zip.
.PARAMETER SkipInstaller
    Do not run Inno Setup.
#>
#Requires -Version 5.1
[CmdletBinding()]
param(
    [switch]$NoBuild,
    [string]$Target = "",
    [string]$OutDir = "",
    [switch]$SkipZip,
    [switch]$SkipInstaller
)

$ErrorActionPreference = "Stop"

# Runs a native program and throws when it fails. The error preference is relaxed while it
# runs so that progress written to stderr (cargo) is not turned into a terminating error.
function Invoke-Tool {
    param(
        [Parameter(Mandatory = $true)][string]$FilePath,
        [string[]]$Arguments = @()
    )
    Write-Host "> $FilePath $($Arguments -join ' ')"
    $saved = $ErrorActionPreference
    $ErrorActionPreference = "Continue"
    try {
        & $FilePath @Arguments
    } finally {
        $ErrorActionPreference = $saved
    }
    if ($LASTEXITCODE -ne 0) {
        throw "$FilePath failed with exit code $LASTEXITCODE"
    }
}

$CallerDir = (Get-Location).Path
$Root = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot "..\..")).Path
& python (Join-Path $Root "packaging/update-notices.py") --check
if ($LASTEXITCODE) { throw "Bundled license notices are missing or stale" }
Set-Location -LiteralPath $Root

# Version from [workspace.package] in the root Cargo.toml.
$Version = ""
$Section = ""
foreach ($Line in Get-Content -LiteralPath (Join-Path $Root "Cargo.toml")) {
    if ($Line -match '^\s*\[([^\]]+)\]') {
        $Section = $Matches[1]
    } elseif ($Section -eq "workspace.package" -and $Line -match '^\s*version\s*=\s*"([^"]+)"') {
        $Version = $Matches[1]
        break
    }
}
if (-not $Version) { throw "cannot read the [workspace.package] version from Cargo.toml" }

# Installer architecture: rustpy.iss supports x86_64 and arm64.
if ($Target) {
    if ($Target -like "x86_64-*") { $Arch = "x86_64" }
    elseif ($Target -like "aarch64-*") { $Arch = "arm64" }
    else { throw "unsupported target '$Target': use an x86_64 or aarch64 Windows target" }
} else {
    if ($env:PROCESSOR_ARCHITECTURE -eq "AMD64") { $Arch = "x86_64" }
    elseif ($env:PROCESSOR_ARCHITECTURE -eq "ARM64") { $Arch = "arm64" }
    else { throw "unsupported host architecture '$env:PROCESSOR_ARCHITECTURE'" }
}

# Relative CARGO_TARGET_DIR values are relative to the repository root (cargo runs there).
$TargetDir = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { "target" }
$TargetDir = [System.IO.Path]::GetFullPath([System.IO.Path]::Combine($Root, $TargetDir))
$ReleaseDir = if ($Target) { Join-Path (Join-Path $TargetDir $Target) "release" } else { Join-Path $TargetDir "release" }
if ($OutDir) {
    $OutDir = [System.IO.Path]::GetFullPath([System.IO.Path]::Combine($CallerDir, $OutDir))
} else {
    $OutDir = Join-Path $TargetDir "windows"
}
New-Item -ItemType Directory -Force -Path $OutDir | Out-Null

# --- 1. release binary -------------------------------------------------------
if (-not $NoBuild) {
    $CargoArgs = @("build", "--locked", "--release", "-p", "rustpy", "--features", "native-web-preview")
    if ($Target) { $CargoArgs += @("--target", $Target) }
    Invoke-Tool "cargo" $CargoArgs
}
$Exe = Join-Path $ReleaseDir "rustpy.exe"
if (-not (Test-Path -LiteralPath $Exe)) { throw "release binary not found: $Exe (run without -NoBuild)" }

# --- 2. icon (optional, needs ImageMagick) -----------------------------------
$IconFile = ""
$Magick = Get-Command "magick" -CommandType Application -ErrorAction SilentlyContinue | Select-Object -First 1
if ($Magick) {
    $IconFile = Join-Path $OutDir "rustpy.ico"
    $Svg = Join-Path $Root "packaging\linux\rustpy.svg"
    try {
        Invoke-Tool $Magick.Path @("-background", "none", "-density", "384", $Svg, "-define", "icon:auto-resize=256,128,64,48,32,16", $IconFile)
    } catch {
        Write-Warning "could not render rustpy.ico with ImageMagick: $_"
        $IconFile = ""
    }
} else {
    # rustpy.iss then picks up packaging\windows\rustpy.ico (committed, rendered from the SVG).
    Write-Host "ImageMagick (magick) not found: the installer uses packaging\windows\rustpy.ico"
}

$NativeLicenses = Join-Path $OutDir "Native-Licenses.zip"
& python (Join-Path $Root "packaging/stage-native-licenses.py") --runtime-file $Exe --output $NativeLicenses
if ($LASTEXITCODE) { throw "Native license/source collection failed" }

# --- 3. portable zip ---------------------------------------------------------
if (-not $SkipZip) {
    $Stage = Join-Path $OutDir "portable"
    if (Test-Path -LiteralPath $Stage) { Remove-Item -LiteralPath $Stage -Recurse -Force }
    New-Item -ItemType Directory -Path $Stage | Out-Null
    Copy-Item -LiteralPath $Exe -Destination $Stage
    Copy-Item -LiteralPath (Join-Path $Root "THIRD-PARTY-NOTICES.txt") -Destination $Stage
    Copy-Item -LiteralPath (Join-Path $Root "LICENSE") -Destination $Stage
    Copy-Item -LiteralPath $NativeLicenses -Destination $Stage
    Set-Content -LiteralPath (Join-Path $Stage "rustpy-portable.cmd") -Encoding Ascii -Value @(
        '@echo off',
        'rem Starts RustPy with its settings in the "config" folder next to this file.',
        'set "RUSTPY_CONFIG_DIR=%~dp0config"',
        'start "" "%~dp0rustpy.exe" %*'
    )
    Set-Content -LiteralPath (Join-Path $Stage "README.txt") -Encoding Ascii -Value @(
        "RustPy $Version for Windows ($Arch), portable build",
        "",
        "rustpy.exe            starts the IDE; settings, logs and local history go to",
        "                      %APPDATA%\rustpy (or to RUSTPY_CONFIG_DIR when it is set).",
        "rustpy-portable.cmd   starts the IDE with RUSTPY_CONFIG_DIR set to the 'config'",
        "                      folder next to it, so the settings travel with this folder.",
        "                      (The window size and position are still remembered in",
        "                      %APPDATA%\rustpy\data.)",
        "",
        "Python is not included: install it from https://www.python.org/ or the Microsoft",
        "Store, then pick the interpreter in Settings > Project > Python Interpreter."
    )
    $Zip = Join-Path $OutDir "rustpy-$Version-windows-$Arch-portable.zip"
    if (Test-Path -LiteralPath $Zip) { Remove-Item -LiteralPath $Zip -Force }
    Compress-Archive -Path (Join-Path $Stage "*") -DestinationPath $Zip
    Write-Host "Portable zip: $Zip"
}

# --- 4. installer (Inno Setup) -------------------------------------------------
if (-not $SkipInstaller) {
    $Iscc = $env:ISCC
    if (-not $Iscc) {
        $Bases = @()
        if ($env:ProgramFiles) { $Bases += $env:ProgramFiles }
        if (${env:ProgramFiles(x86)}) { $Bases += ${env:ProgramFiles(x86)} }
        if ($env:LOCALAPPDATA) { $Bases += (Join-Path $env:LOCALAPPDATA "Programs") }
        foreach ($Base in $Bases) {
            $Candidate = Join-Path $Base "Inno Setup 6\ISCC.exe"
            if (Test-Path -LiteralPath $Candidate) { $Iscc = $Candidate; break }
        }
    }
    if (-not $Iscc) {
        $Command = Get-Command "ISCC.exe" -CommandType Application -ErrorAction SilentlyContinue | Select-Object -First 1
        if ($Command) { $Iscc = $Command.Path }
    }
    if (-not $Iscc) {
        Write-Warning "Inno Setup 6 (ISCC.exe) not found; skipping the installer. Install it from https://jrsoftware.org/isdl.php or set `$env:ISCC."
    } else {
        $IsccArgs = @("/DNativeLicenses=$NativeLicenses", "/DMyAppVersion=$Version", "/DBinDir=$ReleaseDir", "/DOutDir=$OutDir", "/DArch=$Arch")
        if ($IconFile) { $IsccArgs += "/DIconFile=$IconFile" }
        $IsccArgs += (Join-Path $PSScriptRoot "rustpy.iss")
        Invoke-Tool $Iscc $IsccArgs
        Write-Host "Installer: $(Join-Path $OutDir "rustpy-$Version-windows-$Arch-setup.exe")"
    }
}

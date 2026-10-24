# WiX v4/v5 SDK must be installed explicitly by the release operator; never bootstrap tools.
# The feature-selection UI needs the matching UI extension, also installed explicitly:
#   dotnet tool install --global wix --version 5.0.2
#   wix extension add -g WixToolset.UI.wixext/5.0.2
[CmdletBinding()]
param([Parameter(Mandatory=$true)][string]$Target,[string]$OutDir="",[switch]$NoBuild)
$ErrorActionPreference="Stop"
$Root=(Resolve-Path (Join-Path $PSScriptRoot "..\..")).Path
& python (Join-Path $Root "packaging/update-notices.py") --check
if ($LASTEXITCODE) { throw "Bundled license notices are missing or stale" }
$Arch=if($Target -eq "aarch64-pc-windows-msvc"){"arm64"}elseif($Target -eq "x86_64-pc-windows-msvc"){"x64"}else{throw "Unsupported MSI target"}
$VersionLine=(Get-Content (Join-Path $Root "Cargo.toml") | Select-String '^version = "' | Select-Object -First 1).Line
$Version=($VersionLine -split '"')[1]
$MsiVersion=($Version -split '[-+]')[0]
$TargetDir=if($env:CARGO_TARGET_DIR){[IO.Path]::GetFullPath($env:CARGO_TARGET_DIR)}else{Join-Path $Root "target"}
if(-not $OutDir){$OutDir=Join-Path $TargetDir "windows"}
New-Item -ItemType Directory -Force $OutDir | Out-Null
Push-Location $Root
try {
 if(-not $NoBuild){ & cargo build --locked --release -p rustpy --target $Target --features native-web-preview; if($LASTEXITCODE){throw "Cargo failed"} }
 $BinDir=Join-Path $TargetDir "$Target\release"
 $NativeLicenses=Join-Path $OutDir "Native-Licenses.zip"
 & python (Join-Path $Root "packaging/stage-native-licenses.py") --runtime-file (Join-Path $BinDir "rustpy.exe") --output $NativeLicenses
 if($LASTEXITCODE){throw "Native license/source collection failed"}
 $Marker=Join-Path $OutDir "rustpy-install.kind"
 Set-Content -LiteralPath $Marker -Value 'msi' -Encoding Ascii
 $Extensions=(& wix extension list -g) -join "`n"
 if($Extensions -notmatch 'WixToolset\.UI\.wixext'){throw "WixToolset.UI.wixext is not installed; run: wix extension add -g WixToolset.UI.wixext/5.0.2"}
 & wix build (Join-Path $PSScriptRoot "rustpy.wxs") -ext WixToolset.UI.wixext -arch $Arch -d "Version=$MsiVersion" -d "BinDir=$BinDir" -d "Marker=$Marker" -d "NativeLicenses=$NativeLicenses" -d "ProjectLicense=$(Join-Path $Root 'LICENSE')" -d "Notices=$(Join-Path $Root 'THIRD-PARTY-NOTICES.txt')" -o (Join-Path $OutDir "rustpy-$Version-windows-$Arch.msi")
 if($LASTEXITCODE){throw "WiX failed"}
} finally {Pop-Location}

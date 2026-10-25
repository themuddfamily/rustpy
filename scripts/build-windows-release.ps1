<# Build native Windows release artifacts with retained linker and licence evidence. #>
[CmdletBinding()]
param([Parameter(Mandatory=$true)][string]$Target)
$ErrorActionPreference = "Stop"
$Root = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
Set-Location -LiteralPath $Root
$Arch = if ($Target -eq 'x86_64-pc-windows-msvc') { 'x64' } elseif ($Target -eq 'aarch64-pc-windows-msvc') { 'arm64' } else { throw 'Unsupported native Windows release target' }
$Process = [Diagnostics.Process]::GetCurrentProcess()
$Process.PriorityClass = 'BelowNormal'
$Allowed = $Process.ProcessorAffinity.ToInt64()
$Selected = [long]0
$Count = 0
for ($Bit = 0; $Bit -lt 63 -and $Count -lt 2; $Bit++) {
    $Mask = [long]1 -shl $Bit
    if ($Allowed -band $Mask) { $Selected = $Selected -bor $Mask; $Count++ }
}
if (-not $Selected) { throw 'No CPUs available for release build' }
$Process.ProcessorAffinity = [IntPtr]$Selected
$env:CARGO_BUILD_JOBS = '2'
$env:RAYON_NUM_THREADS = '2'
$env:RUST_TEST_THREADS = '2'
$TemporaryBase = if ($env:RUNNER_TEMP) { $env:RUNNER_TEMP } elseif ($env:TMPDIR) { $env:TMPDIR } else { [IO.Path]::GetTempPath() }
$TaskRoot = Join-Path $TemporaryBase 'rustpy-native-release'
New-Item -ItemType Directory -Force -Path $TaskRoot | Out-Null
$env:TMPDIR = $TaskRoot
$env:TMP = $TaskRoot
$env:TEMP = $TaskRoot
$Proof = Join-Path $TaskRoot ([Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $Proof | Out-Null
if ($env:GITHUB_ENV) { "RUSTPY_NATIVE_EVIDENCE_DIR=$Proof" >> $env:GITHUB_ENV }

$VsWhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
$VsJson = (& $VsWhere -all -prerelease -products '*' -format json) -join "`n"
if ($LASTEXITCODE) { throw 'Cannot inventory installed Visual Studio instances' }
$VsJson | Set-Content -LiteralPath (Join-Path $Proof 'vs-instances.json') -Encoding utf8
$Instances = @($VsJson | ConvertFrom-Json)
$Installation = $Instances | Where-Object { $_.installationVersion -match '^17\.' -and $_.isPrerelease -ne $true } | Sort-Object { [version]$_.installationVersion } -Descending | Select-Object -First 1
if (-not $Installation) { $Installation = $Instances | Where-Object { $_.installationVersion -match '^18\.' -and $_.isPrerelease -ne $true } | Sort-Object { [version]$_.installationVersion } -Descending | Select-Object -First 1 }
if (-not $Installation) { throw "A reviewed Visual Studio 2022 or 2026 toolchain is required; discovered: $VsJson" }
$Vs = $Installation.installationPath
$DevCmd = Join-Path $Vs 'Common7\Tools\VsDevCmd.bat'
$CaptureEnvironment = Join-Path $Proof 'capture-vs-environment.cmd'
@('@echo off', "call `"$DevCmd`" -no_logo -arch=$Arch -host_arch=$Arch >nul", 'if errorlevel 1 exit /b %errorlevel%', 'set') | Set-Content -LiteralPath $CaptureEnvironment -Encoding ascii
$Variables = & $env:ComSpec /d /c $CaptureEnvironment
if ($LASTEXITCODE) { throw 'Visual Studio native environment failed' }
foreach ($Line in $Variables) {
    $Pair = $Line -split '=', 2
    if ($Pair.Count -eq 2 -and $Pair[0]) { [Environment]::SetEnvironmentVariable($Pair[0], $Pair[1], 'Process') }
}
$Linker = Join-Path $env:VCToolsInstallDir "bin\Host$Arch\$Arch\link.exe"
if (-not (Test-Path -LiteralPath $Linker)) { throw "Native linker missing: $Linker" }
$RustVersion = (& rustc -vV) -join "`n"
if ($LASTEXITCODE) { throw 'Cannot inspect Rust toolchain' }
$Commit = ($RustVersion -split "`n" | Where-Object { $_ -like 'commit-hash: *' }) -replace '^commit-hash: ', ''
$SourceRevision = (& git rev-parse HEAD).Trim()
if ($LASTEXITCODE -or $SourceRevision -notmatch '^[0-9a-f]{40}$') { throw 'Cannot record the source revision' }
if ($env:CARGO_ENCODED_RUSTFLAGS) {
    $Separator = [string][char]31
    $env:CARGO_ENCODED_RUSTFLAGS = $env:CARGO_ENCODED_RUSTFLAGS + $Separator + '-C' + $Separator + 'target-feature=+crt-static'
} elseif ($env:RUSTFLAGS -notmatch 'target-feature=\+crt-static') {
    $env:RUSTFLAGS = "$env:RUSTFLAGS -C target-feature=+crt-static".Trim()
}
$Toolchain = @{
    WindowsSdkDir=$env:WindowsSdkDir; WindowsSDKVersion=$env:WindowsSDKVersion
    VCToolsInstallDir=$env:VCToolsInstallDir; VCToolsVersion=$env:VCToolsVersion
    vsinstallation=$Vs; vsproductversion=$Installation.installationVersion
    vsproductline=$Installation.catalog.productLineVersion; vsproductid=$Installation.productId
    vsisprerelease=$Installation.isPrerelease; vsiscomplete=$Installation.isComplete; vsislaunchable=$Installation.isLaunchable
    target=$Target; rustc_verbose=$RustVersion; rustc_commit=$Commit
    source_revision=$SourceRevision
    rustflags=$env:RUSTFLAGS; encoded_rustflags=$env:CARGO_ENCODED_RUSTFLAGS
}
$Toolchain | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath (Join-Path $Proof 'toolchain.json') -Encoding utf8
$Python = (Get-Command python -CommandType Application | Select-Object -First 1).Source
$Evidence = Join-Path $Proof 'runtime'
$NativeProducer = Join-Path $Root 'scripts\prepare-windows-native-evidence.py'
& $Python $NativeProducer --preflight --toolchain-record (Join-Path $Proof 'toolchain.json') --output $Evidence
if ($LASTEXITCODE) { throw 'Native licence input preflight failed' }
$Map = Join-Path $Proof 'rustpy.map'
$Recorder = Join-Path $Root 'scripts\record-native-link.py'
$Launcher = Join-Path $Proof 'record-linker.cmd'
@('@echo off', "`"$Python`" `"$Recorder`" --driver `"$Linker`" --directory `"$Proof`" --link-map `"$Map`" --rustc-commit $Commit -- %*") | Set-Content -LiteralPath $Launcher -Encoding ascii
& cargo rustc --locked --release -j 2 -p rustpy --bin rustpy --target $Target --features native-web-preview -- -C "linker=$Launcher" -C "link-arg=/MAP:$Map" -C 'link-arg=/VERBOSE:LIB'
if ($LASTEXITCODE) { throw 'Native Windows release build failed' }
$TargetDir = if ($env:CARGO_TARGET_DIR) { [IO.Path]::GetFullPath($env:CARGO_TARGET_DIR) } else { Join-Path $Root 'target' }
$Binary = Join-Path $TargetDir "$Target\release\rustpy.exe"
& $Python $NativeProducer --binary $Binary --link-map $Map --link-trace (Join-Path $Proof 'link-trace.txt') --link-record (Join-Path $Proof 'link-record.json') --toolchain-record (Join-Path $Proof 'toolchain.json') --output $Evidence
if ($LASTEXITCODE) { throw 'Windows native licence evidence preparation failed' }
$env:RUSTPY_NATIVE_RUNTIME_MANIFEST = Join-Path $Evidence 'runtime-manifest.json'
& (Join-Path $Root 'packaging\windows\build.ps1') -Target $Target -NoBuild
& (Join-Path $Root 'packaging\windows\build-msi.ps1') -Target $Target -NoBuild
if ($LASTEXITCODE) { throw 'Windows packaging failed' }

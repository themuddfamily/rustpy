<#
.SYNOPSIS
    Builds the Windows test package of RustPy: rustpy.exe in the portable layout.

.DESCRIPTION
    For trying a commit by hand, not for distribution. CI's Windows build job runs it for
    every push to main and .github/workflows/test-build.yml re-publishes that artifact (or
    runs this script itself when CI has none), so this is the one definition of the package.

    Compared with scripts/build-windows-release.ps1 it uses a "quick" Cargo profile defined
    on the command line: the release profile (opt-level 3) without link-time optimization or
    debug information and with 16 code-generation units; the C runtime is linked statically
    as in released builds. On a 4-vCPU windows-2025 runner with empty caches, opt-level 2
    built no faster than 3 (about 32 min) and opt-level 1 saved a quarter (24 min), so the
    package keeps the released code generation: its speed is what users get. rust-lld linked
    only 2 s faster than the MSVC linker without LTO, so the default linker stays. It skips
    the installers and the native licence evidence. The output folder holds rustpy.exe,
    rustpy-portable.cmd, LICENSE, THIRD-PARTY-NOTICES.txt and a README that marks it as a
    test build.

    After the build it runs `rustpy.exe --version` and a timed `rustpy.exe inspect` over a
    folder of the checkout as a smoke check; either failing fails the script.

.PARAMETER OptLevel
    opt-level of the workspace crates (dependencies keep the release opt-level 3).
.PARAMETER NoWebPreview
    Build without the native-web-preview feature (released builds have it).
.PARAMETER InspectDir
    Folder of the checkout that the smoke check inspects (default: two small Python
    fixtures of rustpy-debug, so the check stays quick and deterministic).
.PARAMETER Label
    Shown in the package README, e.g. the branch and commit.
#>
[CmdletBinding()]
param(
    [ValidateSet('1', '2', '3')][string]$OptLevel = '3',
    [switch]$NoWebPreview,
    [string]$OutDir = '',
    [string]$InspectDir = 'crates/rustpy-debug/tests/fixtures/scientific',
    [string]$Label = ''
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$Root = Split-Path -Parent $PSScriptRoot
$TargetDir = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { Join-Path $Root 'target' }
if (-not $OutDir) { $OutDir = Join-Path $TargetDir 'windows-test' }

$Version = $null
$Section = ''
foreach ($Line in Get-Content -LiteralPath (Join-Path $Root 'Cargo.toml')) {
    if ($Line -match '^\s*\[([^\]]+)\]') { $Section = $Matches[1]; continue }
    if ($Section -eq 'workspace.package' -and $Line -match '^\s*version\s*=\s*"([^"]+)"') { $Version = $Matches[1]; break }
}
if (-not $Version) { throw 'Cannot read the workspace version from Cargo.toml' }

# Released Windows builds link the C runtime statically; keep the test build the same
# so it runs on a machine without the Visual C++ redistributable.
if ($env:RUSTFLAGS -notmatch 'target-feature=\+crt-static') {
    $env:RUSTFLAGS = "$env:RUSTFLAGS -C target-feature=+crt-static".Trim()
}

$Settings = @(
    'profile.quick.inherits="release"',
    'profile.quick.lto=false',
    'profile.quick.codegen-units=16',
    'profile.quick.debug=0',
    "profile.quick.opt-level=$OptLevel",
    'profile.quick.package."*".opt-level=3'
)
$CargoArgs = @('build', '--locked', '--profile', 'quick', '-p', 'rustpy', '--timings')
foreach ($Setting in $Settings) { $CargoArgs += @('--config', $Setting) }
if (-not $NoWebPreview) { $CargoArgs += @('--features', 'native-web-preview') }

Write-Host "cargo $($CargoArgs -join ' ')"
$Clock = [Diagnostics.Stopwatch]::StartNew()
& cargo @CargoArgs
if ($LASTEXITCODE) { throw "cargo build failed ($LASTEXITCODE)" }
$BuildSeconds = [int]$Clock.Elapsed.TotalSeconds
Write-Host "Build: $BuildSeconds s"
if ($env:GITHUB_OUTPUT) { "build_seconds=$BuildSeconds" >> $env:GITHUB_OUTPUT }

$Exe = Join-Path $TargetDir 'quick\rustpy.exe'
if (-not (Test-Path -LiteralPath $Exe)) { throw "rustpy.exe not found: $Exe" }

# --- portable layout -----------------------------------------------------------
if (Test-Path -LiteralPath $OutDir) { Remove-Item -LiteralPath $OutDir -Recurse -Force }
New-Item -ItemType Directory -Path $OutDir | Out-Null
Copy-Item -LiteralPath $Exe -Destination $OutDir
Copy-Item -LiteralPath (Join-Path $Root 'LICENSE') -Destination $OutDir
Copy-Item -LiteralPath (Join-Path $Root 'THIRD-PARTY-NOTICES.txt') -Destination $OutDir
Set-Content -LiteralPath (Join-Path $OutDir 'rustpy-portable.cmd') -Encoding Ascii -Value @(
    '@echo off',
    'rem Starts RustPy with its settings in the "config" folder next to this file.',
    'set "RUSTPY_CONFIG_DIR=%~dp0config"',
    'start "" "%~dp0rustpy.exe" %*'
)
$Description = if ($Label) { $Label } else { 'local build' }
Set-Content -LiteralPath (Join-Path $OutDir 'README.txt') -Encoding Ascii -Value @(
    "RustPy $Version for Windows (x86_64), TEST BUILD ($Description)",
    '',
    'For trying a change by hand. Not a release and not for redistribution: it is built',
    "without link-time optimization (opt-level $OptLevel), is not signed, and has no",
    'licence evidence bundle published for it as the released packages have.',
    '',
    'rustpy.exe            starts the IDE; settings, logs and local history go to',
    '                      %APPDATA%\rustpy (or to RUSTPY_CONFIG_DIR when it is set).',
    "rustpy-portable.cmd   starts the IDE with RUSTPY_CONFIG_DIR set to the 'config'",
    '                      folder next to it, so a test build does not touch your',
    '                      normal settings.'
)

# --- smoke check -----------------------------------------------------------------
function Invoke-Exe([string[]]$Arguments, [int]$TimeoutSeconds) {
    $Info = [Diagnostics.ProcessStartInfo]::new()
    $Info.FileName = $Exe
    $Info.UseShellExecute = $false
    $Info.CreateNoWindow = $true
    $Info.RedirectStandardOutput = $true
    $Info.RedirectStandardError = $true
    foreach ($Argument in $Arguments) { $Info.ArgumentList.Add($Argument) }
    $Info.Environment['RUSTPY_CONFIG_DIR'] = $SmokeConfig
    $Child = [Diagnostics.Process]::Start($Info)
    $Stdout = $Child.StandardOutput.ReadToEndAsync()
    $Stderr = $Child.StandardError.ReadToEndAsync()
    if (-not $Child.WaitForExit($TimeoutSeconds * 1000)) {
        $Child.Kill($true)
        $Child.WaitForExit()
        throw "rustpy $($Arguments -join ' ') timed out after $TimeoutSeconds s; output so far: $($Stdout.Result) $($Stderr.Result)"
    }
    $Child.WaitForExit()
    [pscustomobject]@{ Code = $Child.ExitCode; Out = $Stdout.Result; Err = $Stderr.Result }
}
$Scratch = if ($env:RUNNER_TEMP) { $env:RUNNER_TEMP } else { [IO.Path]::GetTempPath() }
$SmokeConfig = Join-Path $Scratch 'rustpy-test-build-config'
$Report = Join-Path $Scratch 'rustpy-test-build-inspect'
$SmokeProject = Join-Path $Scratch 'rustpy-test-build-project'
foreach ($Dir in @($SmokeConfig, $Report, $SmokeProject)) {
    if (Test-Path -LiteralPath $Dir) { Remove-Item -LiteralPath $Dir -Recurse -Force }
}

$Result = Invoke-Exe @('--version') 60
if ($Result.Code -ne 0 -or $Result.Out.Trim() -cne "RustPy $Version") {
    throw "rustpy --version failed ($($Result.Code)): $($Result.Out) $($Result.Err)"
}
Write-Host $Result.Out.Trim()

$Clock.Restart()
# A copy outside the checkout keeps the project small: inspecting a folder of the checkout
# still loads the whole workspace, build output included, which took over five minutes.
New-Item -ItemType Directory -Path $SmokeProject | Out-Null
Copy-Item -Path (Join-Path (Join-Path $Root $InspectDir) '*.py') -Destination $SmokeProject
$InspectArgs = @('inspect', $SmokeProject, 'Default', $Report, '-format', 'json', '-v2')
$Result = Invoke-Exe $InspectArgs 180
$InspectSeconds = [Math]::Round($Clock.Elapsed.TotalSeconds, 1)
# 0: no Error-severity problem, 1: some (the checkout has deliberate fixtures), 2: could not run.
if ($Result.Code -notin @(0, 1) -or $Result.Out -notmatch 'Inspecting ') {
    throw "rustpy inspect failed ($($Result.Code)): $($Result.Out) $($Result.Err)"
}
Write-Host $Result.Out.Trim()
Write-Host "Inspect $InspectDir`: $InspectSeconds s"
if ($env:GITHUB_OUTPUT) { "inspect_seconds=$InspectSeconds" >> $env:GITHUB_OUTPUT }
if ($env:GITHUB_STEP_SUMMARY) {
    @(
        "### RustPy test build",
        "",
        "| | |",
        "|---|---|",
        "| version | $Version ($Description) |",
        "| profile | opt-level $OptLevel (dependencies 3), codegen-units 16, no LTO, web preview $(-not $NoWebPreview) |",
        "| cargo build | $BuildSeconds s |",
        "| ``rustpy inspect $InspectDir`` | $InspectSeconds s |"
    ) -join "`n" >> $env:GITHUB_STEP_SUMMARY
}
foreach ($Dir in @($SmokeConfig, $Report, $SmokeProject)) {
    if (Test-Path -LiteralPath $Dir) { Remove-Item -LiteralPath $Dir -Recurse -Force }
}

# RustPy Windows acceptance: the MSIX (Microsoft Store) package.
#
# Run from a normal (non-admin) PowerShell window in the folder holding this
# script, the test-signed package and its certificate:
#
#   powershell -ExecutionPolicy Bypass -File .\run-msix-acceptance.ps1
#
# Files (from packaging/windows/build-msix.sh --sign, see qa/windows/README.md):
#   rustpy-<version>-windows-x64-test-signed.msix   (or -arm64-)
#   rustpy-msix-test.cer                             the throw-away signing certificate
#
# Options:
#   -Package <path>     the .msix to test (default: the newest *-test-signed.msix here)
#   -Gui                also open the real RustPy window through the package for a few
#                       seconds (it takes a screenshot and closes itself)
#   -MachineCert        trust the certificate in LocalMachine\TrustedPeople instead of
#                       CurrentUser\TrustedPeople (needs an elevated PowerShell; use it
#                       when Add-AppxPackage reports 0x800B0109 / an untrusted root)
#   -KeepInstalled      leave the package and certificate installed for manual checks
#
# What it does: trusts the test certificate (in TrustedPeople only, never as a root
# authority), installs the package for the current user with Add-AppxPackage, checks
# Get-AppxPackage, the `rustpy` app execution alias and the declared file associations,
# runs `rustpy --verbose --version` through the alias to prove the process has package
# identity and reports install kind msix (Store-managed updates), then removes the
# package and the certificate again and verifies that nothing is left. Your .py, .pyi
# and .ipynb associations are compared before and after: installing a package only
# offers RustPy in "Open with" and Settings > Default apps, it never takes a type over.
# Results: msix-acceptance-summary.txt and msix-acceptance-report.json next to this
# script; send the summary (or both files) back.

param(
    [string]$Package = '',
    [switch]$Gui,
    [switch]$MachineCert,
    [switch]$KeepInstalled
)

$ErrorActionPreference = 'Continue'
$here = Split-Path -Parent $MyInvocation.MyCommand.Path
$summary = New-Object System.Collections.Generic.List[string]
$report = [ordered]@{}
$fails = 0
$skips = 0
function Note($line) { $summary.Add($line); Write-Host $line }
function Check($ok, $what) {
    if ($ok) { Note "PASS  $what" } else { Note "FAIL  $what"; $script:fails++ }
}
function Skip($what) { Note "SKIP  $what"; $script:skips++ }

# Runs a program with redirected output (no window) and returns code/out/err lines.
function Invoke-Tool([string]$file, [string[]]$arguments, [int]$timeoutMs = 60000, [hashtable]$Environment = @{}) {
    try {
        $info = New-Object System.Diagnostics.ProcessStartInfo
        $info.FileName = $file
        $info.Arguments = (($arguments | ForEach-Object { if ($_ -match '[\s"]') { '"' + ($_ -replace '"', '\"') + '"' } else { $_ } }) -join ' ')
        $info.UseShellExecute = $false
        $info.CreateNoWindow = $true
        $info.RedirectStandardOutput = $true
        $info.RedirectStandardError = $true
        foreach ($k in $Environment.Keys) { $info.EnvironmentVariables[$k] = $Environment[$k] }
        $p = [System.Diagnostics.Process]::Start($info)
        $t1 = $p.StandardOutput.ReadToEndAsync()
        $t2 = $p.StandardError.ReadToEndAsync()
        if (-not $p.WaitForExit($timeoutMs)) {
            try { $p.Kill() } catch {}
            return @{ Code = -1; Out = @(); Err = @("timed out after $timeoutMs ms") }
        }
        $split = { param($t) @("$t" -split "`r?`n" | Where-Object { $_.Trim() }) }
        return @{ Code = $p.ExitCode; Out = (& $split $t1.Result); Err = (& $split $t2.Result) }
    } catch {
        return @{ Code = -1; Out = @(); Err = @("could not run ${file}: $_") }
    }
}

# True when a key and all its subkeys hold no values (Windows leaves such empty
# .ext keys behind after removing a packaged app's file type entries).
function Test-EmptyKey($key) {
    if ($key.ValueCount) { return $false }
    foreach ($n in $key.GetSubKeyNames()) {
        $sub = $key.OpenSubKey($n)
        try { if (-not (Test-EmptyKey $sub)) { return $false } } finally { $sub.Close() }
    }
    return $true
}

# Every value under the HKCU keys that decide what opens .py/.pyi/.ipynb ("<absent>"
# when a key does not exist or holds no values), to prove the package left the user's
# choice alone.
function Association-Snapshot {
    $lines = New-Object System.Collections.Generic.List[string]
    foreach ($ext in '.py', '.pyi', '.ipynb') {
        foreach ($sub in "Software\Classes\$ext", "Software\Microsoft\Windows\CurrentVersion\Explorer\FileExts\$ext\UserChoice") {
            $key = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey($sub)
            if (-not $key) { $lines.Add("$sub <absent>"); continue }
            if (Test-EmptyKey $key) { $key.Close(); $lines.Add("$sub <absent>"); continue }
            try {
                foreach ($n in ($key.GetValueNames() | Sort-Object)) { $lines.Add("$sub [$n] = $($key.GetValue($n))") }
                # Only the default verb matters here; OpenWithProgids lists every candidate
                # app and Windows itself keeps packaged entries there.
            } finally { $key.Close() }
        }
    }
    return , $lines.ToArray()
}

Note "RustPy MSIX acceptance  $(Get-Date -Format s)"
Note "Windows: $([Environment]::OSVersion.VersionString) $env:PROCESSOR_ARCHITECTURE"

# ---------------------------------------------------------------------------
Note "== 1. Package and certificate =="
if (-not $Package) {
    $Package = Get-ChildItem $here -Filter '*-test-signed.msix' | Sort-Object LastWriteTime -Descending | Select-Object -First 1 -ExpandProperty FullName
}
$cerPath = Join-Path $here 'rustpy-msix-test.cer'
if (-not $Package -or -not (Test-Path $Package) -or -not (Test-Path $cerPath)) {
    Note "FAIL  a *-test-signed.msix and rustpy-msix-test.cer must be next to this script"
    $summary | Set-Content -Encoding utf8 (Join-Path $here 'msix-acceptance-summary.txt')
    exit 1
}
Note "package: $Package"
Add-Type -AssemblyName System.IO.Compression.FileSystem
$zip = [IO.Compression.ZipFile]::OpenRead($Package)
try {
    $entry = $zip.GetEntry('AppxManifest.xml')
    $reader = New-Object IO.StreamReader($entry.Open())
    [xml]$manifest = $reader.ReadToEnd()
    $reader.Close()
    $hasSignature = [bool]$zip.GetEntry('AppxSignature.p7x')
} finally { $zip.Dispose() }
$identity = $manifest.Package.Identity
Note "identity: $($identity.Name) $($identity.Version) $($identity.ProcessorArchitecture), publisher $($identity.Publisher)"
$report.identity = @{ name = $identity.Name; version = $identity.Version; arch = $identity.ProcessorArchitecture; publisher = $identity.Publisher }
Check $hasSignature "the package is signed (AppxSignature.p7x)"
$cert = New-Object System.Security.Cryptography.X509Certificates.X509Certificate2($cerPath)
Check ($cert.Subject -eq $identity.Publisher) "certificate subject '$($cert.Subject)' equals the manifest Publisher"
$sig = Get-AuthenticodeSignature -FilePath $Package -ErrorAction SilentlyContinue
if ($sig) { Note "Authenticode status before trusting: $($sig.Status) (expected UnknownError/NotTrusted for a self-signed test certificate)" }

$existing = @(Get-AppxPackage -Name $identity.Name -ErrorAction SilentlyContinue)
if ($existing.Count) {
    Note "FAIL  $($identity.Name) is already installed ($($existing[0].PackageFullName)); not touching it. Remove it first or test on another account."
    $summary | Set-Content -Encoding utf8 (Join-Path $here 'msix-acceptance-summary.txt')
    exit 1
}
$assocBefore = Association-Snapshot
$aliasPath = Join-Path $env:LOCALAPPDATA 'Microsoft\WindowsApps\rustpy.exe'
$aliasBefore = Test-Path $aliasPath
if ($aliasBefore) { Note "note: $aliasPath already exists before the install (another package owns the alias?)" }

$store = if ($MachineCert) { 'Cert:\LocalMachine\TrustedPeople' } else { 'Cert:\CurrentUser\TrustedPeople' }
$certWasThere = Test-Path "$store\$($cert.Thumbprint)"
$imported = $false
if (-not $certWasThere) {
    try {
        Import-Certificate -FilePath $cerPath -CertStoreLocation $store -ErrorAction Stop | Out-Null
        $imported = $true
    } catch { Note "FAIL  importing the test certificate into $store : $_"; $fails++ }
}
Check (Test-Path "$store\$($cert.Thumbprint)") "test certificate trusted in $store (TrustedPeople only; it is not a root authority and is removed at the end)"

# ---------------------------------------------------------------------------
Note "== 2. Install =="
$installed = $null
try {
    Add-AppxPackage -Path $Package -ErrorAction Stop
    $installed = Get-AppxPackage -Name $identity.Name
} catch {
    Note "FAIL  Add-AppxPackage: $($_.Exception.Message)"
    $fails++
    if (-not $MachineCert -and "$($_.Exception.Message)" -match '0x800B0109|0x800B010A|root certificate|trust') {
        Note "  Windows did not accept the certificate from CurrentUser\TrustedPeople on this PC. Run again from an elevated PowerShell with -MachineCert."
    }
}
if ($installed) {
    Note "installed: $($installed.PackageFullName)"
    $report.package = @{ full_name = $installed.PackageFullName; family = $installed.PackageFamilyName; location = $installed.InstallLocation; signature = "$($installed.SignatureKind)"; status = "$($installed.Status)" }
    Check ($installed.Version -eq $identity.Version) "Get-AppxPackage reports version $($installed.Version)"
    Check ("$($installed.Status)" -eq 'Ok') "package status $($installed.Status)"
    Check ("$($installed.InstallLocation)" -match '\\WindowsApps\\') "installed under WindowsApps ($($installed.InstallLocation))"
    Check (Test-Path (Join-Path $installed.InstallLocation 'rustpy.exe')) "rustpy.exe is in the package"

    # --- manifest declarations (as Windows registered them) ---
    $m = Get-AppxPackageManifest -Package $installed.PackageFullName
    $exts = @($m.Package.Applications.Application.Extensions.Extension)
    $types = @($exts | Where-Object Category -eq 'windows.fileTypeAssociation' | ForEach-Object { $_.FileTypeAssociation.SupportedFileTypes.FileType } | ForEach-Object { "$_".Trim() })
    foreach ($t in '.py', '.pyi', '.ipynb') { Check ($types -contains $t) "file type association declared for $t" }
    $aliases = @($exts | Where-Object Category -eq 'windows.appExecutionAlias' | ForEach-Object { $_.AppExecutionAlias.ExecutionAlias.Alias })
    Check ($aliases -contains 'rustpy.exe') "app execution alias rustpy.exe declared"
    Check (@($m.Package.Capabilities.Capability | ForEach-Object { $_.Name }) -contains 'runFullTrust') "runFullTrust capability"

    # --- alias ---
    Check (Test-Path $aliasPath) "alias $aliasPath exists"
    $cmd = @(Get-Command rustpy -All -ErrorAction SilentlyContinue | Where-Object { "$($_.Source)" -like '*\Microsoft\WindowsApps\rustpy.exe' })
    Check ($cmd.Count -gt 0) "Get-Command rustpy resolves the alias ($(@(Get-Command rustpy -All -ErrorAction SilentlyContinue | ForEach-Object Source) -join '; '))"

    # --- package identity and install kind, through the alias ---
    $v = Invoke-Tool $aliasPath @('--verbose', '--version')
    $report.version_output = $v.Out
    Note "rustpy --verbose --version (exit $($v.Code)): $($v.Out -join ' | ')"
    Check ($v.Code -eq 0 -and ($v.Out -match '^RustPy ').Count) "rustpy --version runs through the alias"
    Check (($v.Out -contains 'install: msix')) "RustPy reports install kind msix"
    Check (($v.Out -contains 'package-identity: yes')) "RustPy runs with package identity (GetCurrentPackageFullName)"
    Check (($v.Out -contains 'updates: Microsoft Store')) "self-update is off: updates are managed by the Microsoft Store"

    if ($Gui) {
        $tmp = Join-Path $env:TEMP "rustpy-msix-acceptance-$PID"
        New-Item -ItemType Directory -Force $tmp | Out-Null
        $shot = Join-Path $tmp 'msix-window.png'
        $g = Invoke-Tool $aliasPath @('--screenshot', $shot) 120000 @{
            RUSTPY_CONFIG_DIR = (Join-Path $tmp 'config'); RUSTPY_SYSTEM_DIR = (Join-Path $tmp 'system')
            RUSTPY_SMOKE_DELAY = '3'; RUSTPY_SMOKE_DEADLINE = '60'
        }
        Check ($g.Code -eq 0 -and (Test-Path $shot)) "the RustPy window opened from the package and closed itself (exit $($g.Code), screenshot $(if (Test-Path $shot) { (Get-Item $shot).Length } else { 'missing' }) bytes) $($g.Err | Select-Object -Last 2)"
        Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
    } else {
        Skip "RustPy window under package identity (run again with -Gui; it opens for a few seconds)"
    }
    Get-Process rustpy -ErrorAction SilentlyContinue | Where-Object { "$($_.Path)" -like "$($installed.InstallLocation)*" } | Stop-Process -Force -ErrorAction SilentlyContinue
}

# ---------------------------------------------------------------------------
Note "== 3. Removal =="
if ($KeepInstalled) {
    Skip "removal (-KeepInstalled). Later: Remove-AppxPackage $(if ($installed) { $installed.PackageFullName }); Remove-Item $store\$($cert.Thumbprint)"
} else {
    if ($installed) {
        try { Remove-AppxPackage -Package $installed.PackageFullName -ErrorAction Stop } catch { Note "FAIL  Remove-AppxPackage: $_"; $fails++ }
        Check (-not @(Get-AppxPackage -Name $identity.Name -ErrorAction SilentlyContinue).Count) "Get-AppxPackage no longer lists $($identity.Name)"
        Check ((Test-Path $aliasPath) -eq $aliasBefore) "the rustpy alias is gone again"
    }
    if ($imported) {
        Remove-Item -LiteralPath "$store\$($cert.Thumbprint)" -ErrorAction SilentlyContinue
        Check (-not (Test-Path "$store\$($cert.Thumbprint)")) "test certificate removed from $store"
    }
}
# Remove empty .ext keys Windows left behind where none existed before the test.
foreach ($ext in '.py', '.pyi', '.ipynb') {
    $sub = "Software\Classes\$ext"
    if ($assocBefore -notcontains "$sub <absent>") { continue }
    $key = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey($sub)
    if (-not $key) { continue }
    $empty = Test-EmptyKey $key; $key.Close()
    if ($empty) {
        Remove-Item -LiteralPath "HKCU:\$sub" -Recurse -Force -ErrorAction SilentlyContinue
        Note "  removed the empty HKCU\$sub key Windows left after uninstalling the package"
    }
}
$assocAfter = Association-Snapshot
$diff = @(Compare-Object $assocBefore $assocAfter | ForEach-Object { "$($_.SideIndicator) $($_.InputObject)" })
$report.association_difference = $diff
Check (-not $diff.Count) "your .py/.pyi/.ipynb associations are unchanged $(if ($diff.Count) { '(' + ($diff -join '; ') + ')' })"

Note ""
Note "Result: $fails failed, $skips skipped"
$summary | Set-Content -Encoding utf8 (Join-Path $here 'msix-acceptance-summary.txt')
$report.summary = $summary.ToArray()
$report | ConvertTo-Json -Depth 6 | Set-Content -Encoding utf8 (Join-Path $here 'msix-acceptance-report.json')
exit ([int]($fails -gt 0))

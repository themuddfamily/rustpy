# RustPy Windows acceptance: interpreter discovery through the Windows registry,
# the py launcher, and WSL distributions.
#
# Run from a normal (non-admin) PowerShell window in the folder holding this
# script and windows_acceptance.exe:
#
#   powershell -ExecutionPolicy Bypass -File .\run-windows-acceptance.ps1
#
# Optional: -Distro Ubuntu   (probe only that WSL distribution)
#
# It changes nothing on the machine. It reads the registry, runs `py -0p`,
# `wsl --list`, and `python3 --version` inside each WSL distribution, then
# runs RustPy's real discovery code and compares. Results are written to
# windows-acceptance-report.json and windows-acceptance-summary.txt next to
# this script; send the summary (or both files) back.

param([string[]]$Distro = @())

$ErrorActionPreference = 'Continue'
$here = Split-Path -Parent $MyInvocation.MyCommand.Path
$exe = Join-Path $here 'windows_acceptance.exe'
$summary = New-Object System.Collections.Generic.List[string]
$fails = 0

function Note($line) { $summary.Add($line); Write-Host $line }
function Check($ok, $what) {
    if ($ok) { Note "PASS  $what" } else { Note "FAIL  $what"; $script:fails++ }
}
function Norm($p) {
    if (-not $p) { return $null }
    try { $p = [System.IO.Path]::GetFullPath($p.Trim().Trim('"')) } catch {}
    return $p.TrimEnd('\').ToLowerInvariant()
}

# wsl.exe attached to this console can leave it without newline auto-return
# (later lines "staircase"). Every external tool runs with redirected streams
# and a hidden console of its own, and the console mode is restored after
# RustPy's own run as well.
try {
    Add-Type -Namespace RustPyQa -Name ConsoleMode -MemberDefinition @'
[System.Runtime.InteropServices.DllImport("kernel32.dll")] public static extern System.IntPtr GetStdHandle(int n);
[System.Runtime.InteropServices.DllImport("kernel32.dll")] public static extern bool GetConsoleMode(System.IntPtr h, out uint mode);
[System.Runtime.InteropServices.DllImport("kernel32.dll")] public static extern bool SetConsoleMode(System.IntPtr h, uint mode);
'@ -ErrorAction Stop
    $consoleModeApi = $true
} catch { $consoleModeApi = $false }
function Save-ConsoleMode {
    if (-not $consoleModeApi) { return $null }
    $modes = @{}
    foreach ($n in -10, -11, -12) {
        $m = [uint32]0
        if ([RustPyQa.ConsoleMode]::GetConsoleMode([RustPyQa.ConsoleMode]::GetStdHandle($n), [ref]$m)) { $modes[$n] = $m }
    }
    return $modes
}
function Restore-ConsoleMode($modes) {
    if (-not $modes) { return }
    foreach ($n in $modes.Keys) { [void][RustPyQa.ConsoleMode]::SetConsoleMode([RustPyQa.ConsoleMode]::GetStdHandle($n), $modes[$n]) }
}
function Quote-Arg([string]$a) {
    if ($a -and $a -notmatch '[\s"]') { return $a }
    return '"' + ($a -replace '(\\*)"', '$1$1\"' -replace '(\\+)$', '$1$1') + '"'
}
# Runs a tool with no access to this console; returns its stdout (UTF-16LE
# output, as wsl.exe --list emits, is detected) followed by stderr lines.
function Invoke-Captured([string]$file, [string[]]$arguments, [int]$timeoutMs = 60000) {
    $saved = Save-ConsoleMode
    try {
        $info = New-Object System.Diagnostics.ProcessStartInfo
        $info.FileName = $file
        $info.Arguments = (($arguments | ForEach-Object { Quote-Arg $_ }) -join ' ')
        $info.UseShellExecute = $false
        $info.CreateNoWindow = $true
        $info.RedirectStandardInput = $true
        $info.RedirectStandardOutput = $true
        $info.RedirectStandardError = $true
        $p = [System.Diagnostics.Process]::Start($info)
        $p.StandardInput.Close()
        $out = New-Object System.IO.MemoryStream
        $err = New-Object System.IO.MemoryStream
        $t1 = $p.StandardOutput.BaseStream.CopyToAsync($out)
        $t2 = $p.StandardError.BaseStream.CopyToAsync($err)
        if (-not $p.WaitForExit($timeoutMs)) { try { $p.Kill() } catch {} ; return @("timed out after $timeoutMs ms") }
        [void]$t1.Wait(5000); [void]$t2.Wait(5000)
        $decode = {
            param($bytes)
            if ($bytes.Length -ge 2 -and ($bytes -contains 0)) { [System.Text.Encoding]::Unicode.GetString($bytes) }
            else { [System.Text.Encoding]::UTF8.GetString($bytes) }
        }
        $text = (& $decode $out.ToArray()) + "`n" + (& $decode $err.ToArray())
        return @($text -split "`r?`n" | ForEach-Object { $_.Trim([char]0, [char]0xFEFF, ' ') } | Where-Object { $_ })
    } catch {
        return @("could not run ${file}: $_")
    } finally {
        Restore-ConsoleMode $saved
    }
}

Note "RustPy Windows acceptance  $(Get-Date -Format s)"
Note "Windows $([System.Environment]::OSVersion.VersionString)"
Note ""

# --- Ground truth: py launcher ---------------------------------------------
$launcher = @()
$py = Get-Command py.exe -ErrorAction SilentlyContinue
if ($py) {
    $raw = & py.exe -0p 2>$null
    foreach ($l in $raw) {
        if ($l -match '([A-Za-z]:\\.*?\.exe)') { $launcher += $Matches[1] }
    }
    Note "py launcher: $($py.Source)  ($($launcher.Count) interpreters)"
} else {
    Note "py launcher: not installed (skipping launcher checks)"
}

# --- Ground truth: registry (PEP 514) ---------------------------------------
$registry = @()
foreach ($hive in 'HKCU:\Software\Python', 'HKLM:\Software\Python', 'HKLM:\Software\WOW6432Node\Python') {
    if (-not (Test-Path $hive)) { continue }
    foreach ($key in Get-ChildItem $hive -Recurse -ErrorAction SilentlyContinue | Where-Object { $_.PSChildName -eq 'InstallPath' }) {
        $props = Get-ItemProperty $key.PSPath -ErrorAction SilentlyContinue
        $exePath = $props.ExecutablePath
        if (-not $exePath -and $props.'(default)') { $exePath = Join-Path $props.'(default)' 'python.exe' }
        if ($exePath -and (Test-Path $exePath)) { $registry += $exePath }
    }
}
Note "registry (PEP 514): $($registry.Count) installed interpreters"

# --- Ground truth: WSL --------------------------------------------------------
$wslTruth = @()
$wsl = Get-Command wsl.exe -ErrorAction SilentlyContinue
if ($wsl) {
    $wslTruth = @(Invoke-Captured $wsl.Source @('--list', '--quiet'))
    Note "WSL distributions: $($wslTruth -join ', ')"
} else {
    Note "WSL: not installed (skipping WSL checks)"
}
$probeDistros = if ($Distro.Count) { $Distro } else { $wslTruth }
$wslPython = @{}
foreach ($d in $probeDistros) {
    $v = Invoke-Captured $wsl.Source @('--distribution', $d, '--exec', 'python3', '--version') | Select-Object -First 1
    $wslPython[$d] = "$v".Trim()
    Note "  $d : python3 -> $($wslPython[$d])"
}
Note ""

# --- RustPy's real discovery -------------------------------------------------
if (-not (Test-Path $exe)) { Note "FAIL  windows_acceptance.exe not found next to this script"; exit 1 }
$exeArgs = @(); foreach ($d in $probeDistros) { $exeArgs += '--distro'; $exeArgs += $d }
Note "Running RustPy discovery (can take up to a few minutes with several WSL distributions)..."
$savedMode = Save-ConsoleMode
$json = & $exe @exeArgs 2>"$here\windows-acceptance-stderr.txt"
Restore-ConsoleMode $savedMode
$json | Out-File -Encoding utf8 "$here\windows-acceptance-report.json"
try { $report = $json | Out-String | ConvertFrom-Json } catch {
    Note "FAIL  could not parse RustPy's report (see windows-acceptance-stderr.txt)"; exit 1
}
Note "RustPy discovery took $($report.discovery_ms) ms and found $(@($report.discovered).Count) interpreters:"
foreach ($i in $report.discovered) { Note ("  {0,-8} {1,-22} {2}" -f $i.version, $i.kind, $i.path) }
Note ""

$found = @{}
foreach ($i in $report.discovered) { $found[(Norm $i.path)] = $i }

Note "== py launcher =="
if (-not $py) { Note "SKIP  no py launcher" }
foreach ($p in $launcher) { Check ($found.ContainsKey((Norm $p))) "launcher interpreter discovered: $p" }

Note "== Windows registry =="
if (-not $registry.Count) { Note "SKIP  no PEP 514 registrations" }
foreach ($p in $registry) { Check ($found.ContainsKey((Norm $p))) "registry interpreter discovered: $p" }

Note "== WSL =="
if ($wsl) {
    $theirs = @($report.wsl_registrations)
    Check (($theirs -join '|') -eq ($wslTruth -join '|')) "WSL distributions listed: RustPy [$($theirs -join ', ')] vs Windows [$($wslTruth -join ', ')]"
    foreach ($probe in $report.wsl_probes) {
        $truth = $wslPython[$probe.distro]
        if ($truth -match 'Python (\d+\.\d+\.\d+)') {
            $want = $Matches[1]
            Check ($probe.ok -and $probe.version -eq $want) "WSL '$($probe.distro)' python3 probed: RustPy $($probe.version) $($probe.error) vs $want ($($probe.ms) ms)"
        } else {
            # Invalid for the right reason: the remote python3 is missing or
            # failed, never a local spawn/adapter failure (e.g. cmd refusing
            # the probe's arguments: Spawn("batch file arguments are invalid")).
            $err = "$($probe.error)"
            $local = $err -match 'Spawn\(|batch file|transport adapter|Required tool|is a batch file'
            $remote = $err -match 'python3|not found|No such file|\(Failed\)'
            Check ((-not $probe.ok) -and (-not $local) -and $remote) "WSL '$($probe.distro)' has no python3 ($truth) and RustPy reports the missing remote interpreter, not a local launch failure: $err"
        }
    }
} else { Note "SKIP  WSL not installed" }

Note ""
Note ("RESULT: " + $(if ($fails) { "$fails check(s) FAILED" } else { "all checks passed" }))
$summary | Out-File -Encoding utf8 "$here\windows-acceptance-summary.txt"
Write-Host "`nSaved windows-acceptance-summary.txt and windows-acceptance-report.json in $here"
exit $fails

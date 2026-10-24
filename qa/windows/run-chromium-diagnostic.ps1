# RustPy Windows diagnostic: why "Open in browser" with Edge/Chrome loads
# nothing. Launches the same headless browser command in several ways (plain,
# job objects with and without breakaway, the Run tool's session, ConPTY, the
# fixed launcher session) and records, for each, whether the page was
# requested, the first process's exit, the processes it started and
# Chromium's own log.
#
# Run from a normal (non-admin) PowerShell window in the folder holding this
# script and windows_chromium_diagnostic.exe:
#
#   powershell -ExecutionPolicy Bypass -File .\run-chromium-diagnostic.ps1
#
# Options:
#   -Browsers edge,chrome   which browsers (default both, when installed)
#   -Modes plain,launcher   a subset of the modes (default all; the list is in
#                           the report's "modes")
#   -Timeout 20             seconds to wait for each page load
#   -KeepTemp               keep the temporary folder (profiles and logs)
#
# Takes up to about 10 minutes with both browsers. Browsers run headless with
# throw-away profiles in one folder under %TEMP% that is deleted at the end;
# nothing is shown on the desktop. Results: chromium-diagnostic-summary.txt and
# chromium-diagnostic-report.json next to this script; send both back.

param(
    [string]$Browsers = 'edge,chrome',
    [string]$Modes = '',
    [int]$Timeout = 20,
    [switch]$KeepTemp
)

$ErrorActionPreference = 'Continue'
$here = Split-Path -Parent $MyInvocation.MyCommand.Path
$summary = New-Object System.Collections.Generic.List[string]
$fullReport = [ordered]@{}

function Note($line) { $summary.Add($line); Write-Host $line }

# Console-mode guard: a child that leaves the console in another mode must
# not break this window's line output.
try {
    Add-Type -Namespace RustPyQaDiag -Name ConsoleMode -MemberDefinition @'
[System.Runtime.InteropServices.DllImport("kernel32.dll")] public static extern System.IntPtr GetStdHandle(int n);
[System.Runtime.InteropServices.DllImport("kernel32.dll")] public static extern bool GetConsoleMode(System.IntPtr h, out uint mode);
[System.Runtime.InteropServices.DllImport("kernel32.dll")] public static extern bool SetConsoleMode(System.IntPtr h, uint mode);
'@ -ErrorAction Stop
    $consoleModeApi = $true
} catch { $consoleModeApi = $false }
function Save-ConsoleMode {
    if (-not $consoleModeApi) { return $null }
    $saved = @{}
    try {
        foreach ($n in -10, -11, -12) {
            $m = [uint32]0
            if ([RustPyQaDiag.ConsoleMode]::GetConsoleMode([RustPyQaDiag.ConsoleMode]::GetStdHandle($n), [ref]$m)) { $saved[$n] = $m }
        }
    } catch {}
    return $saved
}
function Restore-ConsoleMode($saved) {
    if (-not $saved) { return }
    try { foreach ($n in $saved.Keys) { [void][RustPyQaDiag.ConsoleMode]::SetConsoleMode([RustPyQaDiag.ConsoleMode]::GetStdHandle($n), $saved[$n]) } } catch {}
}
function Quote-Arg([string]$a) {
    if ($a -and $a -notmatch '[\s"]') { return $a }
    return '"' + ($a -replace '(\\*)"', '$1$1\"' -replace '(\\+)$', '$1$1') + '"'
}
# Runs a tool with redirected streams and no console window of its own;
# returns @{ Code; Out (string); Err (string[]) }.
function Invoke-Tool([string]$file, [string[]]$arguments, [int]$timeoutMs) {
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
        if (-not $p.WaitForExit($timeoutMs)) {
            try { $p.Kill() } catch {}
            return @{ Code = -1; Out = ''; Err = @("timed out after $timeoutMs ms") }
        }
        [void]$t1.Wait(5000); [void]$t2.Wait(5000)
        $errText = [System.Text.Encoding]::UTF8.GetString($err.ToArray())
        return @{
            Code = $p.ExitCode
            Out = [System.Text.Encoding]::UTF8.GetString($out.ToArray())
            Err = @($errText -split "`r?`n" | Where-Object { $_ })
        }
    } catch {
        return @{ Code = -1; Out = ''; Err = @("could not run ${file}: $_") }
    } finally {
        Restore-ConsoleMode $saved
    }
}
function Processes-Matching([string]$text) {
    $t = $text.ToLowerInvariant()
    return @(Get-CimInstance Win32_Process -ErrorAction SilentlyContinue | Where-Object { $_.CommandLine -and $_.CommandLine.ToLowerInvariant().Contains($t) })
}

# An Image File Execution Options "Debugger" for a browser's image name
# (MSEdgeRedirect registers one for msedge.exe) makes Windows start that
# program instead of the browser. Returns the Debugger command line or $null.
# With UseFilter set, a subkey whose FilterFullPath is $path applies first.
function Get-IfeoRedirect([string]$image, [string[]]$paths) {
    $key = "HKLM:\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Image File Execution Options\$image"
    try { $own = Get-ItemProperty -LiteralPath $key -ErrorAction Stop } catch { return $null }
    if ($own.UseFilter) {
        $want = @($paths | Where-Object { $_ } | ForEach-Object { "$_".Trim().Trim('"').Replace('/', '\').ToLowerInvariant() })
        foreach ($sub in @(Get-ChildItem -LiteralPath $key -ErrorAction SilentlyContinue)) {
            $v = Get-ItemProperty -LiteralPath $sub.PSPath -ErrorAction SilentlyContinue
            if (-not $v -or -not "$($v.Debugger)".Trim() -or -not $v.FilterFullPath) { continue }
            if ($want -contains "$($v.FilterFullPath)".Trim().Trim('"').Replace('/', '\').ToLowerInvariant()) { return "$($v.Debugger)".Trim() }
        }
    }
    if ("$($own.Debugger)".Trim()) { return "$($own.Debugger)".Trim() }
    return $null
}
function Redirect-Reason([string]$browser, [string]$debugger) {
    $d = $debugger.Trim()
    if ($d.StartsWith('"')) { $exe = $d.Substring(1).Split('"')[0] }
    elseif ($d -match '^(.*?\.exe)') { $exe = $Matches[1] }
    else { $exe = ($d -split '\s+')[0] }
    $exe = Split-Path -Leaf $exe
    return "$browser launches are redirected by $exe (Image File Execution Options); RustPy cannot open pages in $browser on this machine."
}

$exe = Join-Path $here 'windows_chromium_diagnostic.exe'
$tmp = Join-Path ([System.IO.Path]::GetTempPath()) ('rustpy-chromium-diag-' + [guid]::NewGuid().ToString('N').Substring(0, 8))
New-Item -ItemType Directory -Force -Path $tmp | Out-Null
$saved = Save-ConsoleMode
try {
    $os = Get-CimInstance Win32_OperatingSystem -ErrorAction SilentlyContinue
    $identity = [Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()
    $isAdmin = $identity.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
    Note "RustPy Chromium launch diagnostic  $(Get-Date -Format s)"
    Note "Windows: $($os.Caption) $($os.Version) build $($os.BuildNumber); PowerShell $($PSVersionTable.PSVersion); elevated: $isAdmin"
    $fullReport.windows = [ordered]@{ caption = $os.Caption; version = $os.Version; build = $os.BuildNumber; elevated = $isAdmin }
    foreach ($b in @(
            @{ Name = 'Edge'; Keys = @('HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\App Paths\msedge.exe', 'HKCU:\SOFTWARE\Microsoft\Windows\CurrentVersion\App Paths\msedge.exe') },
            @{ Name = 'Chrome'; Keys = @('HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\App Paths\chrome.exe', 'HKCU:\SOFTWARE\Microsoft\Windows\CurrentVersion\App Paths\chrome.exe') })) {
        $path = $null
        foreach ($k in $b.Keys) { try { $path = (Get-ItemProperty -LiteralPath $k -ErrorAction Stop).'(default)'; if ($path) { break } } catch {} }
        $version = $null
        if ($path) { try { $version = (Get-Item -LiteralPath $path.Trim('"') -ErrorAction Stop).VersionInfo.ProductVersion } catch {} }
        Note "$($b.Name): $(if ($path) { "$path ($version)" } else { 'not registered' })"
        $image = if ($b.Name -eq 'Edge') { 'msedge.exe' } else { 'chrome.exe' }
        $debugger = Get-IfeoRedirect $image @($(if ($path) { [Environment]::ExpandEnvironmentVariables($path.Trim('"')) }))
        if ($debugger) {
            Note "  Image File Execution Options Debugger for ${image}: $debugger"
            Note "  SKIP  $(Redirect-Reason $b.Name $debugger)"
        }
        $fullReport["$($b.Name.ToLowerInvariant())_registered"] = [ordered]@{ path = $path; version = $version; ifeo_debugger = $debugger }
        # A browser already running for this user decides hand-off behaviour.
        $name = if ($b.Name -eq 'Edge') { 'msedge' } else { 'chrome' }
        $running = @(Get-Process -Name $name -ErrorAction SilentlyContinue).Count
        Note "  $name processes already running before the diagnostic: $running"
        $fullReport["$($b.Name.ToLowerInvariant())_running_before"] = $running
    }

    if (-not (Test-Path $exe)) {
        Note "FAIL  windows_chromium_diagnostic.exe not found next to this script"
    } else {
        $arguments = @('--work', $tmp, '--browsers', $Browsers, '--timeout', "$Timeout")
        if ($Modes) { $arguments += @('--modes', $Modes) }
        Note "Running the diagnostic (up to about 10 minutes)..."
        $r = Invoke-Tool $exe $arguments 1800000
        $r.Err | Out-File -Encoding utf8 (Join-Path $here 'chromium-diagnostic-stderr.txt')
        $report = $null
        try { $report = $r.Out | ConvertFrom-Json } catch {}
        if (-not $report) {
            Note "FAIL  the diagnostic did not print a report (exit $($r.Code)): $(($r.Err | Select-Object -Last 5) -join ' | ')"
        } else {
            $fullReport.diagnostic = $report
            Note "Helper elevation: $($report.elevation.elevated) ($($report.elevation.elevation_type)); helper in a job: $($report.helper_job.in_job) $($report.helper_job.limit_flags)"
            Note ''
            Note ('{0,-8} {1,-24} {2,-6} {3,-6} {4,-9} {5,-10} {6,-6} {7,-6} {8}' -f 'browser', 'mode', 'page', 'js', 'load ms', 'leader', 'seen', 'alive', 'left')
            foreach ($run in $report.runs) {
                if ($run.redirected_by) { Note ('{0,-8} SKIP  {1}' -f $run.browser, $run.skipped); continue }
                if ($run.skipped) { Note ('{0,-8} skipped: {1}' -f $run.browser, $run.skipped); continue }
                if ($run.error) { Note ('{0,-8} {1,-24} error: {2}' -f $run.browser, $run.mode, $run.error); continue }
                $leader = if ($null -ne $run.leader_exit_code) { "$($run.leader_exit_code)@$($run.leader_exit_ms)" } else { 'running' }
                $load = if ($run.loaded) { $run.beacon_ms } else { '-' }
                Note ('{0,-8} {1,-24} {2,-6} {3,-6} {4,-9} {5,-10} {6,-6} {7,-6} {8}' -f $run.browser, $run.mode, $run.page_requested, $run.javascript_beacon, $load, $leader, @($run.processes_seen).Count, @($run.alive_at_end).Count, @($run.leftovers).Count)
            }
            Note ''
            Note '(leader = first process exit code@ms or running; seen = processes of the run observed; alive = still running at the end of the wait; left = still running after cleanup)'
            foreach ($p in $report.summary.PSObject.Properties) {
                foreach ($n in $p.Value.notes) { Note "NOTE  $($p.Name): $n" }
            }
            foreach ($run in $report.runs) {
                if ($run.skipped -or $run.error -or $run.loaded) { continue }
                Note ''
                Note "--- $($run.browser) $($run.mode): not loaded; processes seen:"
                foreach ($s in @($run.processes_seen)) {
                    Note ("  pid {0} ppid {1} {2} type={3} by={4} job={5} at {6} ms" -f $s.pid, $s.ppid, $s.exe, $s.type, $s.found_by, $s.in_any_job, $s.first_seen_ms)
                }
                if ($run.session_output_tail) { Note "  session output: $(@($run.session_output_tail) -join ' | ')" }
                Note '  chrome_debug.log (last lines):'
                foreach ($l in @($run.chrome_debug_log_tail | Select-Object -Last 15)) { Note "    $l" }
            }
        }
    }
} finally {
    Restore-ConsoleMode $saved
    # Nothing of the diagnostic may keep running.
    $left = Processes-Matching $tmp
    foreach ($p in $left) {
        Note "LEFTOVER  pid $($p.ProcessId) $($p.Name) (stopped now)"
        try { Stop-Process -Id $p.ProcessId -Force -ErrorAction Stop } catch {}
    }
    $fullReport.leftover_processes = @($left | ForEach-Object { "$($_.ProcessId) $($_.Name)" })
    if (-not $left.Count) { Note 'No diagnostic process left running.' }
    $fullReport | ConvertTo-Json -Depth 14 | Out-File -Encoding utf8 (Join-Path $here 'chromium-diagnostic-report.json')
    if ($KeepTemp) {
        Note "Temporary folder kept: $tmp"
    } else {
        Start-Sleep -Milliseconds 500
        Remove-Item -LiteralPath $tmp -Recurse -Force -ErrorAction SilentlyContinue
        if (Test-Path -LiteralPath $tmp) {
            Start-Sleep -Seconds 3
            Remove-Item -LiteralPath $tmp -Recurse -Force -ErrorAction SilentlyContinue
        }
        if (Test-Path -LiteralPath $tmp) { Note "Could not remove $tmp" }
    }
    $summary | Out-File -Encoding utf8 (Join-Path $here 'chromium-diagnostic-summary.txt')
    Write-Host "Results: $(Join-Path $here 'chromium-diagnostic-summary.txt') and chromium-diagnostic-report.json"
}

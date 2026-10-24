# RustPy Windows acceptance, pack 2: the Add Interpreter dialog's environment
# flows, Web Browsers and Preview (detection, Open in browser, Open URL,
# WebView2 built-in preview) and the MSI package.
#
# Run from a normal (non-admin) PowerShell window in the folder holding this
# script and the files of the pack:
#
#   powershell -ExecutionPolicy Bypass -File .\run-windows-acceptance-2.ps1
#
# Options:
#   -Python C:\Python314\python.exe   base interpreter for new environments
#                                     (default: C:\Python314\python.exe when it
#                                     exists, else the py launcher's default)
#   -Distro Ubuntu      also add that WSL distribution's python3 as an interpreter
#   -Gui                also open the real RustPy window (about 20 s, closes
#                       itself) to check the WebView2 built-in preview
#   -SkipMsiInstall     build and validate the MSI but do not install it
#   -KeepTemp           keep the temporary folder for inspection
#
# Everything happens in one folder under %TEMP% that is deleted at the end.
# Browsers run headless with throw-away profiles. When no RustPy installation
# exists, the MSI is installed per user into that temporary folder with the
# file-association and context-menu features off (your .py/.pyi/.ipynb keys are
# compared before and after) and uninstalled; a second, full install that
# associates the files runs only when no per-user .py/.pyi/.ipynb association
# exists (otherwise it is skipped and the exact manual command is printed). Any
# registry or PATH difference left by an uninstall is reported and restored.
# Results: windows-acceptance-2-summary.txt and windows-acceptance-2-report.json
# next to this script; send the summary (or both files) back.

param(
    [string]$Python = '',
    [string]$Distro = '',
    [switch]$Gui,
    [switch]$SkipMsiInstall,
    [switch]$KeepTemp
)

$ErrorActionPreference = 'Continue'
$here = Split-Path -Parent $MyInvocation.MyCommand.Path
$summary = New-Object System.Collections.Generic.List[string]
$fails = 0
$skips = 0
$fullReport = [ordered]@{}

function Note($line) { $summary.Add($line); Write-Host $line }
function Check($ok, $what) {
    if ($ok) { Note "PASS  $what" } else { Note "FAIL  $what"; $script:fails++ }
}
function Skip($what) { Note "SKIP  $what"; $script:skips++ }
function Norm($p) {
    if (-not $p) { return $null }
    $p = "$p".Trim().Trim('"')
    if ($p.StartsWith('\\?\')) { $p = $p.Substring(4) }
    try { $p = [System.IO.Path]::GetFullPath($p) } catch {}
    return $p.TrimEnd('\', '/').ToLowerInvariant()
}
function Same($a, $b) { return ((Norm $a) -and ((Norm $a) -eq (Norm $b))) }
function Under($child, $parent) {
    $c = Norm $child; $p = Norm $parent
    $sep = [string][System.IO.Path]::DirectorySeparatorChar
    return ($c -and $p -and ($c -eq $p -or $c.StartsWith($p + $sep)))
}

# Console-mode guard (wsl.exe and some tools leave the console without
# newline auto-return; see run-windows-acceptance.ps1).
try {
    Add-Type -Namespace RustPyQa2 -Name ConsoleMode -MemberDefinition @'
[System.Runtime.InteropServices.DllImport("kernel32.dll")] public static extern System.IntPtr GetStdHandle(int n);
[System.Runtime.InteropServices.DllImport("kernel32.dll")] public static extern bool GetConsoleMode(System.IntPtr h, out uint mode);
[System.Runtime.InteropServices.DllImport("kernel32.dll")] public static extern bool SetConsoleMode(System.IntPtr h, uint mode);
'@ -ErrorAction Stop
    $consoleModeApi = $true
} catch { $consoleModeApi = $false }
function Save-ConsoleMode {
    if (-not $consoleModeApi) { return $null }
    $modes = @{}
    try {
        foreach ($n in -10, -11, -12) {
            $m = [uint32]0
            if ([RustPyQa2.ConsoleMode]::GetConsoleMode([RustPyQa2.ConsoleMode]::GetStdHandle($n), [ref]$m)) { $modes[$n] = $m }
        }
    } catch {}
    return $modes
}
function Restore-ConsoleMode($modes) {
    if (-not $modes) { return }
    try { foreach ($n in $modes.Keys) { [void][RustPyQa2.ConsoleMode]::SetConsoleMode([RustPyQa2.ConsoleMode]::GetStdHandle($n), $modes[$n]) } } catch {}
}
function Quote-Arg([string]$a) {
    if ($a -and $a -notmatch '[\s"]') { return $a }
    return '"' + ($a -replace '(\\*)"', '$1$1\"' -replace '(\\+)$', '$1$1') + '"'
}
# Runs a tool with redirected streams and no console of its own; returns
# @{ Code; Out (string[]); Err (string[]) }.
# Ground-truth Python calls print UTF-8 (paths contain a non-ASCII letter);
# the RustPy helpers run with the environment unchanged (-AsIs).
function Invoke-Tool([string]$file, [string[]]$arguments, [int]$timeoutMs = 120000, [switch]$AsIs, [string]$WorkDir = '') {
    $saved = Save-ConsoleMode
    try {
        $info = New-Object System.Diagnostics.ProcessStartInfo
        if (-not $AsIs) { $info.EnvironmentVariables['PYTHONIOENCODING'] = 'utf-8' }
        $info.FileName = $file
        $info.Arguments = (($arguments | ForEach-Object { Quote-Arg $_ }) -join ' ')
        $info.UseShellExecute = $false
        if ($WorkDir) { $info.WorkingDirectory = $WorkDir }
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
            return @{ Code = -1; Out = @(); Err = @("timed out after $timeoutMs ms") }
        }
        [void]$t1.Wait(5000); [void]$t2.Wait(5000)
        $decode = {
            param($bytes)
            if ($bytes.Length -ge 2 -and $bytes[1] -eq 0) { [System.Text.Encoding]::Unicode.GetString($bytes) }
            else { [System.Text.Encoding]::UTF8.GetString($bytes) }
        }
        $split = { param($t) @($t -split "`r?`n" | ForEach-Object { $_.Trim([char]0, [char]0xFEFF, ' ') } | Where-Object { $_ }) }
        return @{ Code = $p.ExitCode; Out = (& $split (& $decode $out.ToArray())); Err = (& $split (& $decode $err.ToArray())) }
    } catch {
        return @{ Code = -1; Out = @(); Err = @("could not run ${file}: $_") }
    } finally {
        Restore-ConsoleMode $saved
    }
}
# One line per value of an HKCU key and all its subkeys (or "<absent>"), to prove
# that an install/uninstall left a key exactly as it was.
function Registry-Snapshot([string]$subKey) {
    $lines = New-Object System.Collections.Generic.List[string]
    $walk = {
        param($key, $name)
        foreach ($v in @($key.GetValueNames() | Sort-Object)) {
            $kind = $key.GetValueKind($v)
            $data = $key.GetValue($v, $null, [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)
            if ($data -is [array]) { $data = ($data | ForEach-Object { "$_" }) -join ',' }
            $lines.Add("$name [$v] $kind = $data")
        }
        foreach ($s in @($key.GetSubKeyNames() | Sort-Object)) {
            $child = $key.OpenSubKey($s)
            if ($child) { try { $lines.Add("$name\$s <key>"); & $walk $child "$name\$s" } finally { $child.Close() } }
            else { $lines.Add("$name\$s <unreadable>") }
        }
    }
    $root = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey($subKey)
    if (-not $root) { return @("$subKey <absent>") }
    try { $lines.Add("$subKey <key>"); & $walk $root $subKey } finally { $root.Close() }
    return @($lines)
}
# Runs a pack helper that prints one JSON document; returns the parsed object.
function Invoke-Helper([string]$name, [string[]]$arguments, [int]$timeoutMs) {
    $exe = Join-Path $here $name
    if (-not (Test-Path $exe)) { Note "FAIL  $name not found next to this script"; $script:fails++; return $null }
    $r = Invoke-Tool $exe $arguments $timeoutMs -AsIs
    $r.Err | Out-File -Encoding utf8 (Join-Path $here ("windows-acceptance-2-" + [IO.Path]::GetFileNameWithoutExtension($name) + "-stderr.txt"))
    try { return (($r.Out -join "`n") | ConvertFrom-Json) } catch {
        Note "FAIL  $name did not print a report (exit $($r.Code)): $(($r.Err | Select-Object -Last 5) -join ' | ')"
        $script:fails++
        return $null
    }
}
function Python-Version([string]$python) {
    $r = Invoke-Tool $python @('-c', "import sys;print('%d.%d.%d' % sys.version_info[:3])") 60000
    return "$($r.Out | Select-Object -First 1)".Trim()
}
function Step($report, [string]$name) { return @($report.steps | Where-Object { $_.name -eq $name }) | Select-Object -First 1 }
function Read-Cfg([string]$dir) {
    $cfg = @{}
    $file = Join-Path $dir 'pyvenv.cfg'
    if (Test-Path -LiteralPath $file) {
        foreach ($l in Get-Content -LiteralPath $file) {
            if ($l -match '^\s*([^=]+?)\s*=\s*(.*)$') { $cfg[$Matches[1].ToLowerInvariant()] = $Matches[2].Trim() }
        }
    }
    return $cfg
}
function Processes-Matching([string]$text) {
    $t = $text.ToLowerInvariant()
    return @(Get-CimInstance Win32_Process -ErrorAction SilentlyContinue | Where-Object { $_.CommandLine -and $_.CommandLine.ToLowerInvariant().Contains($t) })
}

$stamp = Get-Date -Format 'yyyyMMdd-HHmmss'
# %LOCALAPPDATA%\Temp rather than %TEMP%, which can be an 8.3 short path.
$tempRoot = Join-Path $env:LOCALAPPDATA 'Temp'
if (-not (Test-Path -LiteralPath $tempRoot)) { $tempRoot = [System.IO.Path]::GetTempPath() }
$tmp = Join-Path $tempRoot "rustpy-acceptance-2-$stamp-$PID"
New-Item -ItemType Directory -Force $tmp | Out-Null

Note "RustPy Windows acceptance 2  $(Get-Date -Format s)"
Note "Windows $([System.Environment]::OSVersion.VersionString)"
$packInfo = Join-Path $here 'pack-info.txt'
if (Test-Path $packInfo) { foreach ($l in Get-Content $packInfo) { Note "pack: $l" } }
Note "temporary folder: $tmp"
Note ""

# The user's own RustPy catalogue must not change (the helpers use the temp folder).
$userCatalogue = Join-Path $env:APPDATA 'rustpy\interpreter-registry.json'
$catalogueBefore = if (Test-Path -LiteralPath $userCatalogue) { (Get-Item -LiteralPath $userCatalogue).LastWriteTimeUtc.Ticks } else { 0 }

$msiInstalled = $false
$msiPath = $null
try {
# ============================================================================
Note "== 1. Add Interpreter: environments (Virtualenv / existing / system / py launcher / uv / WSL) =="
if (-not $Python) {
    if (Test-Path 'C:\Python314\python.exe') { $Python = 'C:\Python314\python.exe' }
    else {
        $pyl = Get-Command py.exe -ErrorAction SilentlyContinue
        if ($pyl) { $Python = "$((Invoke-Tool $pyl.Source @('-c', 'import sys;print(sys.executable)')).Out | Select-Object -First 1)".Trim() }
    }
}
if (-not $Python -or -not (Test-Path -LiteralPath $Python)) {
    Note "FAIL  no base Python found (pass -Python C:\path\python.exe)"; $fails++
} else {
    $baseVersion = Python-Version $Python
    Note "base interpreter: $Python ($baseVersion)"
    $helperArgs = @('--work', (Join-Path $tmp 'interp'), '--base', $Python)
    if ($Distro) { $helperArgs += @('--distro', $Distro) }
    Note "Creating environments through RustPy's environment manager (a minute or two)..."
    $ir = Invoke-Helper 'windows_interpreter_acceptance.exe' $helperArgs 1800000
    $fullReport['interpreters'] = $ir
    if ($ir) {
        $project = $ir.project
        $venv = Join-Path $project '.venv'
        $venvPy = Join-Path $venv 'Scripts\python.exe'

        $s = Step $ir 'new_virtualenv'
        Check ($s.ok) "Virtualenv > New with requirements.txt created and probed ($($s.ms) ms) $($s.error)"
        Check (Test-Path -LiteralPath $venvPy) "Scripts\python.exe exists in the new environment ($venvPy)"
        Check (Same $s.interpreter.path $venvPy) "selected interpreter is Scripts\python.exe: $($s.interpreter.path)"
        Check ($s.interpreter.is_venv -and $s.prefix_matches_location) "probe reports a virtual environment rooted at the chosen location (prefix $($s.interpreter.prefix))"
        Check ($s.interpreter.version -eq $baseVersion) "environment version $($s.interpreter.version) matches the base interpreter ($baseVersion)"
        $cfg = Read-Cfg $venv
        Check ((Same $cfg['home'] (Split-Path -Parent $Python)) -and $cfg['include-system-site-packages'] -eq 'false') "pyvenv.cfg: home = $($cfg['home']), include-system-site-packages = $($cfg['include-system-site-packages'])"
        if (Test-Path -LiteralPath $venvPy) {
            $imp = Invoke-Tool $venvPy @('-c', 'import rustpy_win_probe as m;print(m.VALUE);print(m.__file__)') 60000
            Check (($imp.Out | Select-Object -First 1) -eq '42' -and (Under ($imp.Out | Select-Object -Last 1) $venv)) "offline wheel from requirements.txt installed into the environment: $($imp.Out -join ' ') $($imp.Err -join ' ')"
            $show = Invoke-Tool $venvPy @('-m', 'pip', 'show', 'rustpy-win-probe') 120000
            Check (($show.Out -contains 'Version: 1.0')) "pip show rustpy-win-probe reports Version: 1.0"
            $pv = Invoke-Tool $venvPy @('-m', 'pip', '--version') 120000
            $pvLine = "$($pv.Out | Select-Object -First 1)"
            Check ($pv.Code -eq 0 -and $pvLine -match '^pip \d' -and ($pvLine.ToLowerInvariant().Contains((Norm $venv)))) "pip --version runs from the environment: $pvLine"
            Check ("$($s.pip_version.stdout)".Trim() -eq $pvLine.Trim()) "RustPy saw the same pip: $($s.pip_version.stdout)"
        }

        $s = Step $ir 'existing_virtualenv'
        Check ($s.ok -and $s.same_as_created) "Virtualenv > Existing selects the same interpreter ($($s.ms) ms) $($s.error)"

        foreach ($sys in @($ir.system_interpreters)) {
            $p = $sys.name.Substring(7)
            $truth = Python-Version $p
            Check ($sys.ok -and $sys.interpreter.version -eq $truth) "System interpreter / Reload paths: $p -> RustPy $($sys.interpreter.version) vs Windows $truth $($sys.error)"
        }
        if (-not @($ir.system_interpreters).Count) { Note "FAIL  no system interpreter discovered"; $fails++ }

        $s = Step $ir 'py_launcher_base'
        if ($s.skipped) { Skip "py launcher as base: $($s.skipped)" } else {
            $pyTruth = Python-Version $s.launcher
            $loc = Join-Path $s.location 'Scripts\python.exe'
            Check ($s.ok -and (Test-Path -LiteralPath $loc) -and $s.interpreter.version -eq $pyTruth) "py launcher as base interpreter: new environment $($s.interpreter.version) vs py default $pyTruth $($s.error)"
        }

        $s = Step $ir 'inherit_site_packages'
        $cfg = Read-Cfg $s.location
        Check ($s.ok -and $cfg['include-system-site-packages'] -eq 'true') "Inherit global site-packages: pyvenv.cfg include-system-site-packages = $($cfg['include-system-site-packages']) $($s.error)"

        $s = Step $ir 'existing_location_refused'
        Check ($s.ok) "an existing location is refused before anything runs: $($s.error)"

        $s = Step $ir 'missing_base_reports_error'
        Check ($s.ok -and $s.error) "a missing base interpreter fails with a message ($($s.ms) ms): $($s.error)"

        $s = Step $ir 'cancel'
        Start-Sleep -Seconds 2
        $left = Processes-Matching '.venv-cancel'
        Check ($s.ok -and $s.ms -lt 60000 -and -not $left.Count) "Cancel while python -m venv runs ends the task in $($s.ms) ms and leaves no process ($($left.Count) left): $($s.error)"
        foreach ($p in $left) { try { Stop-Process -Id $p.ProcessId -Force -ErrorAction SilentlyContinue } catch {} }

        $s = Step $ir 'new_uv'
        if ($s.skipped) { Skip "uv > New: $($s.skipped)" } else {
            Check ($s.ok -and (Test-Path -LiteralPath (Join-Path $project '.venv-uv\Scripts\python.exe'))) "uv > New ($($s.uv)) created Scripts\python.exe ($($s.ms) ms) $($s.error)"
        }

        $s = Step $ir 'wsl'
        if ($Distro) {
            if ($s) {
                Check ($s.ok) "WSL '$Distro' python3 added through Add Interpreter > WSL: $($s.interpreter.version) $($s.error)"
                if ($s.ok) { Check ($s.reselected.ok -and $s.reselected.interpreter.version -eq $s.interpreter.version) "the WSL interpreter is selected again from its configured entry: $($s.reselected.interpreter.version) $($s.reselected.error)" }
            } else { Note "FAIL  no WSL step in the report"; $fails++ }
        }

        $s = Step $ir 'catalogue'
        if ($s) {
            Check ($s.ok) "catalogue: named with a non-ASCII name, associated with the project, SDK saved, renamed, removed and hidden (saved $($s.saved), name '$($s.reloaded_name)', renamed '$($s.renamed)', removed $($s.removed), hidden $($s.hidden)) $($s.save_error)"
            Check (Under $s.registry_file $tmp) "the catalogue written is the isolated one: $($s.registry_file)"
        } else { Note "FAIL  catalogue step missing (environment creation failed)"; $fails++ }
    }
}
$catalogueAfter = if (Test-Path -LiteralPath $userCatalogue) { (Get-Item -LiteralPath $userCatalogue).LastWriteTimeUtc.Ticks } else { 0 }
Check ($catalogueBefore -eq $catalogueAfter) "your own RustPy interpreter catalogue ($userCatalogue) was not touched"
Note ""

# ============================================================================
Note "== 2. Web Browsers and Preview =="
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
$browsers = [ordered]@{ Edge = @('msedge.exe', 'Microsoft\Edge\Application\msedge.exe'); Chrome = @('chrome.exe', 'Google\Chrome\Application\chrome.exe'); Firefox = @('firefox.exe', 'Mozilla Firefox\firefox.exe') }
$truth = @{}
$redirects = [ordered]@{}
foreach ($name in $browsers.Keys) {
    $image, $rel = $browsers[$name]
    $paths = @()
    foreach ($hive in 'HKCU:', 'HKLM:') {
        $key = "$hive\Software\Microsoft\Windows\CurrentVersion\App Paths\$image"
        if (Test-Path -LiteralPath $key) {
            $v = (Get-Item -LiteralPath $key).GetValue('')
            if ($v) { $v = [Environment]::ExpandEnvironmentVariables("$v".Trim('"')); if (Test-Path -LiteralPath $v) { $paths += $v } }
        }
    }
    foreach ($base in $env:LOCALAPPDATA, $env:ProgramFiles, ${env:ProgramFiles(x86)}) {
        if ($base) { $c = Join-Path $base $rel; if (Test-Path -LiteralPath $c) { $paths += $c } }
    }
    $truth[$name] = @($paths | Select-Object -Unique)
    Note ("Windows reports {0}: {1}" -f $name, $(if ($truth[$name].Count) { $truth[$name] -join '; ' } else { 'not installed' }))
    $debugger = Get-IfeoRedirect $image $truth[$name]
    if ($debugger) {
        $redirects[$name] = Redirect-Reason $name $debugger
        Note "  Image File Execution Options Debugger for ${image}: $debugger"
        Note "  $($redirects[$name])"
    }
}
$fullReport['ifeo_redirects'] = $redirects
$wv2 = $null
foreach ($k in 'HKLM:\SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}', 'HKCU:\Software\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}') {
    if (Test-Path -LiteralPath $k) { $v = (Get-Item -LiteralPath $k).GetValue('pv'); if ($v -and $v -ne '0.0.0.0') { $wv2 = $v } }
}
Note "WebView2 runtime: $(if ($wv2) { $wv2 } else { 'not found' })"

$rustpyExe = Join-Path $here 'rustpy.exe'
$bArgs = @('--work', (Join-Path $tmp 'browser'))
if ($Gui) {
    if (Test-Path $rustpyExe) { $bArgs += @('--gui', $rustpyExe) } else { Note "FAIL  -Gui needs rustpy.exe next to this script"; $fails++ }
}
Note "Opening pages in headless browsers through RustPy's launcher$(if ($Gui) { ' and the RustPy window (WebView2 preview, ~20 s)' })..."
Note "(each browser has 60 s to load the page and 20 s to stop; the whole step is capped at 5 minutes)"
$br = Invoke-Helper 'windows_browser_acceptance.exe' $bArgs 300000
$fullReport['browsers'] = $br
if ($br) {
    foreach ($name in $browsers.Keys) {
        $d = $br.detection.$name
        if ($truth[$name].Count) {
            $hit = $false; foreach ($t in $truth[$name]) { if (Same $t $d.program) { $hit = $true } }
            Check ($d.exists -and $hit) "$name detected: RustPy resolves '$($d.configured)' to $($d.program)"
        } else {
            Check (-not $d.exists) "$name not installed and RustPy does not invent it ($($d.program))"
        }
    }
    Check ("$($br.system_plan)".StartsWith('Ok(System(')) "System browser entry hands the URL to Windows: $($br.system_plan)"
    Check (@($br.rejected_urls | Where-Object { -not $_ }).Count -eq 0) "javascript:, credential and control-character URLs are refused"
    if ($br.preview_server_error) { Note "FAIL  preview host did not start: $($br.preview_server_error)"; $fails++ }
    foreach ($run in @($br.open_in_browser)) {
        if ($run.skipped) { Skip "Open in browser with $($run.browser): $($run.skipped)"; continue }
        if ($redirects[$run.browser]) { Skip "Open in browser with $($run.browser): $($redirects[$run.browser])"; continue }
        Check ($run.loaded -and $run.javascript -and $run.stylesheet) "Open in browser with $($run.browser): page from the preview host loaded, JavaScript ran, stylesheet applied ($($run.load_ms) ms) $($run.error)"
        if (-not $run.loaded) { Note "  running before Stop: $($run.running_before_stop), status $($run.status); output: $(@($run.output_tail) -join ' | ')"; Note "  run run-chromium-diagnostic.ps1 for the launch details" }
        Check ($run.url_is_one_argument) "$($run.browser): the URL is passed as one argument"
        Start-Sleep -Seconds 1
        $left = Processes-Matching $run.profile_dir
        Check ($run.stopped -and -not $left.Count) "$($run.browser): Stop ended the browser's process tree ($($left.Count) processes left, status $($run.status))"
        foreach ($p in $left) { try { Stop-Process -Id $p.ProcessId -Force -ErrorAction SilentlyContinue } catch {} }
    }
    if ($br.open_url -and $redirects[$br.open_url.browser]) {
        Skip "Open URL with $($br.open_url.browser): $($redirects[$br.open_url.browser])"
    } elseif ($br.open_url) {
        $u = $br.open_url
        Check ($u.loaded -and $u.request -eq $u.expected_target) "Open URL with $($u.browser): the browser requested exactly $($u.expected_target) (got $($u.request))"
        Start-Sleep -Seconds 1
        $left = Processes-Matching $u.profile_dir
        Check ($u.stopped -and -not $left.Count) "Open URL: Stop ended the browser ($($left.Count) processes left)"
        foreach ($p in $left) { try { Stop-Process -Id $p.ProcessId -Force -ErrorAction SilentlyContinue } catch {} }
    } elseif ($br.open_url_skipped) { Skip "Open URL: $($br.open_url_skipped)"
    } elseif ($redirects.Count) { Skip "Open URL: $(@($redirects.Values) -join ' ')"
    } else { Skip "Open URL: no supported browser installed" }
    if ($Gui) {
        $w = $br.webview2
        if (-not $wv2) { Note "note: WebView2 runtime not found in the registry; the preview needs it" }
        Check ($w.loaded -and $w.javascript -and $w.stylesheet) "WebView2 built-in preview in the RustPy window loaded the page, ran JavaScript, applied CSS ($($w.load_ms) ms, UA: $($w.user_agent)) $($w.error)"
        Check ("$($w.user_agent)" -match 'Edg/') "the built-in preview is the Edge WebView2 engine"
        Check ($w.user_data_in_system_dir -and -not $w.user_data_beside_exe) "WebView2 keeps its data in RustPy's system folder, not beside rustpy.exe"
        Check (-not $w.killed_after_timeout) "the RustPy window closed itself after the check (exit $($w.exit))"
        if (-not $w.loaded) { foreach ($l in @($w.log)) { Note "  log: $l" } }
        $orphans = Processes-Matching (Join-Path $tmp 'browser')
        $orphans = @($orphans | Where-Object { $_.Name -match 'rustpy|msedgewebview2' })
        Check (-not $orphans.Count) "no RustPy or WebView2 process left ($($orphans.Count))"
        foreach ($p in $orphans) { try { Stop-Process -Id $p.ProcessId -Force -ErrorAction SilentlyContinue } catch {} }
    } else {
        Skip "WebView2 built-in preview (run again with -Gui; it opens the RustPy window for about 20 seconds)"
    }
}
Note ""

# ============================================================================
Note "== 3. MSI package =="
$wix = Get-Command wix -ErrorAction SilentlyContinue
$dotnet = Get-Command dotnet -ErrorAction SilentlyContinue
$wxs = Join-Path $here 'rustpy.wxs'
$notices = Join-Path $here 'THIRD-PARTY-NOTICES.txt'
if (-not $wix) {
    if ($dotnet) {
        Skip "WiX is not installed. Install it with:  dotnet tool install --global wix --version 5.0.2   (then open a new PowerShell window and run this script again)"
    } else {
        Skip "WiX and the .NET SDK are not installed. Install them with:  winget install Microsoft.DotNet.SDK.8   then   dotnet tool install --global wix --version 5.0.2   (new PowerShell window, then run this script again)"
    }
} elseif (-not (Test-Path $rustpyExe) -or -not (Test-Path $wxs)) {
    Note "FAIL  rustpy.exe or rustpy.wxs missing from the pack"; $fails++
} else {
    $wixVersion = "$((Invoke-Tool $wix.Source @('--version')).Out | Select-Object -First 1)"
    Note "WiX: $($wix.Source) $wixVersion"
    $version = '0.1.0'
    if (Test-Path $packInfo) { foreach ($l in Get-Content $packInfo) { if ($l -match '^version:\s*(\S+)') { $version = ($Matches[1] -split '[-+]')[0] } } }
    $msiDir = Join-Path $tmp 'msi'
    New-Item -ItemType Directory -Force $msiDir | Out-Null
    $marker = Join-Path $msiDir 'rustpy-install.kind'
    Set-Content -LiteralPath $marker -Value 'msi' -Encoding Ascii
    $msiPath = Join-Path $msiDir "rustpy-$version-windows-x64.msi"
    # The feature-selection UI comes from the WixToolset.UI.wixext extension, which must
    # match the WiX version. Use a global copy when there is one; otherwise add it to the
    # temporary folder only (wix extension add without -g), so nothing outside it changes.
    $uiExt = 'WixToolset.UI.wixext'
    $extVersion = if ($wixVersion -match '^(\d+\.\d+\.\d+)') { $Matches[1] } else { '5.0.2' }
    $extList = Invoke-Tool $wix.Source @('extension', 'list', '-g')
    if (@($extList.Out | Where-Object { $_ -match ([regex]::Escape($uiExt) + '\s+' + [regex]::Escape($extVersion)) }).Count) {
        Note "WiX UI extension: $uiExt $extVersion (global)"
    } else {
        $extAdd = Invoke-Tool $wix.Source @('extension', 'add', "$uiExt/$extVersion") 600000 -WorkDir $msiDir
        Check ($extAdd.Code -eq 0) "wix extension add $uiExt/$extVersion (into the temporary folder): exit $($extAdd.Code) $(($extAdd.Out + $extAdd.Err | Select-Object -Last 3) -join ' | ')"
    }
    $build = Invoke-Tool $wix.Source @('build', $wxs, '-ext', $uiExt, '-arch', 'x64', '-d', "Version=$version", '-d', "BinDir=$here", '-d', "Marker=$marker", '-d', "Notices=$notices", '-o', $msiPath) 600000 -WorkDir $msiDir
    $build.Out + $build.Err | Out-File -Encoding utf8 (Join-Path $msiDir 'build.txt')
    $built = ($build.Code -eq 0 -and (Test-Path -LiteralPath $msiPath))
    Check $built "wix build rustpy.wxs -ext $uiExt -> $(Split-Path -Leaf $msiPath) (exit $($build.Code)) $(($build.Out + $build.Err | Select-Object -Last 4) -join ' | ')"
    if (-not $built -and (($build.Out + $build.Err) -join ' ') -match 'EULA|OSMF') {
        Note "  this WiX release requires accepting its EULA; to stay on WiX 5 run:  dotnet tool update --global wix --version 5.0.2"
    }
    if ($built) {
        $val = Invoke-Tool $wix.Source @('msi', 'validate', $msiPath) 600000
        $val.Out + $val.Err | Out-File -Encoding utf8 (Join-Path $msiDir 'validate.txt')
        $iceErrors = @(($val.Out + $val.Err) | Where-Object { $_ -match 'error' })
        $iceWarnings = @(($val.Out + $val.Err) | Where-Object { $_ -match 'warning' })
        Check ($val.Code -eq 0) "ICE validation (wix msi validate): exit $($val.Code), $($iceErrors.Count) error line(s), $($iceWarnings.Count) warning line(s)"
        foreach ($l in ($iceErrors + $iceWarnings | Select-Object -First 15)) { Note "  $l" }

        # Inspect the package tables through Windows Installer itself.
        $optional = @('FileAssociations', 'ContextMenu', 'AddToPath')
        try {
            $installer = New-Object -ComObject WindowsInstaller.Installer
            # COM late binding rejects PowerShell-wrapped values (DISP_E_TYPEMISMATCH):
            # pass plain [string]/[int] arguments.
            $db = $installer.GetType().InvokeMember('OpenDatabase', 'InvokeMethod', $null, $installer, [object[]]@([string]$msiPath, [int]0))
            # Returns one string per row: the first $cols columns joined with '|'.
            $query = {
                param($sql, [int]$cols = 1)
                $view = $db.GetType().InvokeMember('OpenView', 'InvokeMethod', $null, $db, [object[]]@([string]$sql))
                [void]$view.GetType().InvokeMember('Execute', 'InvokeMethod', $null, $view, $null)
                $rows = @()
                while ($true) {
                    $rec = $view.GetType().InvokeMember('Fetch', 'InvokeMethod', $null, $view, $null)
                    if (-not $rec) { break }
                    $vals = @()
                    for ($i = 1; $i -le $cols; $i++) {
                        $vals += [string]$rec.GetType().InvokeMember('StringData', 'GetProperty', $null, $rec, [object[]]@([int]$i))
                    }
                    [void][System.Runtime.InteropServices.Marshal]::FinalReleaseComObject($rec)
                    $rows += ($vals -join '|')
                }
                [void]$view.GetType().InvokeMember('Close', 'InvokeMethod', $null, $view, $null)
                [void][System.Runtime.InteropServices.Marshal]::FinalReleaseComObject($view)
                return $rows
            }
            $sinfo = $db.GetType().InvokeMember('SummaryInformation', 'GetProperty', $null, $db, [object[]]@([int]0))
            $wordCount = [int]$sinfo.GetType().InvokeMember('Property', 'GetProperty', $null, $sinfo, [object[]]@([int]15))
            Check (($wordCount -band 8) -ne 0) "per-user package: no elevation required (Word Count $wordCount)"
            # Non-advertised ProgIds (Advertise is off) are written to the Registry
            # table; there is no Extension table, so look for the class keys there.
            $regKeys = @(& $query 'SELECT `Key` FROM `Registry`')
            $ext = @('py', 'pyi', 'ipynb' | Where-Object { $e = $_; @($regKeys | Where-Object { $_ -match ('(^|\\)\.' + $e + '$') }).Count -gt 0 })
            Check ($ext.Count -eq 3) "file associations in the package (Registry table class keys): .$($ext -join ', .')"
            $files = & $query 'SELECT `FileName` FROM `File`'
            Check (@($files | Where-Object { $_ -match 'rustpy\.exe' }).Count -eq 1) "rustpy.exe is in the File table ($($files -join ', '))"
            $shortcuts = & $query 'SELECT `Name` FROM `Shortcut`'
            Check (@($shortcuts).Count -ge 1) "Start menu shortcut in the package ($($shortcuts -join ', '))"

            # Features: a required Core (no "absent" choice) and three optional children.
            $features = @{}
            foreach ($r in @(& $query 'SELECT `Feature`, `Feature_Parent`, `Level`, `Attributes` FROM `Feature`' 4)) {
                $f = $r -split '\|'; $features[$f[0]] = @{ Parent = $f[1]; Level = $f[2]; Attributes = [int]("0$($f[3])") }
            }
            Note "  Feature table: $(($features.Keys | Sort-Object | ForEach-Object { "$_(parent=$($features[$_].Parent), level=$($features[$_].Level), attributes=$($features[$_].Attributes))" }) -join '; ')"
            $core = $features['Core']
            Check ($core -and -not $core.Parent -and $core.Level -eq '1' -and ($core.Attributes -band 16)) "feature Core: top level, installed by default, cannot be deselected (UIDisallowAbsent)"
            $badOptional = @($optional | Where-Object { $o = $features[$_]; -not ($o -and $o.Parent -eq 'Core' -and $o.Level -eq '1' -and -not ($o.Attributes -band 16)) })
            Check (-not $badOptional.Count) "optional features under Core, on by default and deselectable: $($optional -join ', ') $(if ($badOptional.Count) { "(wrong: $($badOptional -join ', '))" })"
            $conditions = @(& $query 'SELECT `Feature_`, `Level`, `Condition` FROM `Condition`' 3)
            $props = @{ FileAssociations = 'ASSOCIATE_FILES'; ContextMenu = 'CONTEXT_MENU'; AddToPath = 'ADD_TO_PATH' }
            $missingCond = @($optional | Where-Object { $o = $_; -not @($conditions | Where-Object { $c = $_ -split '\|'; $c[0] -eq $o -and $c[1] -eq '0' -and $c[2] -match $props[$o] }).Count })
            Check (-not $missingCond.Count) "convenience properties disable the features (Condition table: $($conditions -join '; '))"
            # Each resource belongs to the right feature.
            $regFeat = @(& $query 'SELECT `Registry`.`Key`, `FeatureComponents`.`Feature_` FROM `Registry`, `FeatureComponents` WHERE `Registry`.`Component_` = `FeatureComponents`.`Component_`' 2)
            $wrongAssoc = @($regFeat | Where-Object { $k, $f = $_ -split '\|'; ($k -match '(^|\\)(\.(py|pyi|ipynb)|RustPy\.(Python|PythonStub|Notebook))(\\|$)') -and $f -ne 'FileAssociations' })
            $assocRows = @($regFeat | Where-Object { ($_ -split '\|')[0] -match '(^|\\)\.(py|pyi|ipynb)$' })
            Check ($assocRows.Count -ge 3 -and -not $wrongAssoc.Count) "the .py/.pyi/.ipynb associations and ProgIds belong to FileAssociations $(if ($wrongAssoc.Count) { "(wrong: $($wrongAssoc -join ', '))" })"
            $menuRows = @($regFeat | Where-Object { ($_ -split '\|')[0] -match '\\shell\\RustPy(\\|$)' })
            Check ($menuRows.Count -ge 6 -and -not @($menuRows | Where-Object { ($_ -split '\|')[1] -ne 'ContextMenu' }).Count) "the Explorer context menu keys belong to ContextMenu ($($menuRows.Count) values)"
            $envRows = @(& $query 'SELECT `Environment`.`Name`, `FeatureComponents`.`Feature_` FROM `Environment`, `FeatureComponents` WHERE `Environment`.`Component_` = `FeatureComponents`.`Component_`' 2)
            Check ($envRows.Count -ge 1 -and -not @($envRows | Where-Object { ($_ -split '\|')[1] -ne 'AddToPath' }).Count) "the PATH entry belongs to AddToPath ($($envRows -join ', '))"
            $coreRows = @(& $query 'SELECT `Shortcut`.`Name`, `FeatureComponents`.`Feature_` FROM `Shortcut`, `FeatureComponents` WHERE `Shortcut`.`Component_` = `FeatureComponents`.`Component_`' 2)
            $coreRows += @(& $query 'SELECT `File`.`FileName`, `FeatureComponents`.`Feature_` FROM `File`, `FeatureComponents` WHERE `File`.`Component_` = `FeatureComponents`.`Component_`' 2)
            Check ($coreRows.Count -ge 2 -and -not @($coreRows | Where-Object { ($_ -split '\|')[-1] -ne 'Core' }).Count) "rustpy.exe, the notices and the Start menu shortcut belong to Core ($($coreRows -join ', '))"
            $dialogs = @(& $query 'SELECT `Dialog` FROM `Dialog`')
            Check (@($dialogs | Where-Object { $_ -eq 'CustomizeDlg' }).Count -eq 1) "interactive installs offer the feature tree (CustomizeDlg in the Dialog table)"
        } catch { Note "FAIL  could not inspect the MSI tables: $_"; $fails++ }
        finally {
            # Release Windows Installer's handle on the .msi so the temporary folder can be removed.
            foreach ($o in @($sinfo, $db, $installer)) { if ($o) { try { [void][System.Runtime.InteropServices.Marshal]::FinalReleaseComObject($o) } catch {} } }
            $sinfo = $null; $db = $null; $installer = $null
            [System.GC]::Collect(); [System.GC]::WaitForPendingFinalizers()
        }

        # ---- per-user install / uninstall into the temporary folder ----
        $upgradeCode = '{418E653B-A5FC-4AA9-82F3-25A3AADF7EA4}'
        $installer = New-Object -ComObject WindowsInstaller.Installer
        $related = { @($installer.RelatedProducts($upgradeCode)) }
        # Windows Installer feature state of the installed product: 3 local, 2 absent.
        $featureState = {
            param($feature)
            $pc = @(& $related) | Select-Object -First 1
            if (-not $pc) { return -1 }
            return [int]$installer.GetType().InvokeMember('FeatureState', 'GetProperty', $null, $installer, [object[]]@([string]$pc, [string]$feature))
        }
        $classes = 'HKCU:\Software\Classes'
        $progIds = @("$classes\RustPy.Python", "$classes\RustPy.PythonStub", "$classes\RustPy.Notebook")
        $menuKeys = @("$classes\*\shell\RustPy", "$classes\Directory\shell\RustPy", "$classes\Directory\Background\shell\RustPy")
        $owned = @("HKCU:\Software\RustPy", "$classes\Applications\rustpy.exe") + $menuKeys + $progIds
        $assoc = @("$classes\.py", "$classes\.pyi", "$classes\.ipynb")
        $assocSub = @('Software\Classes\.py', 'Software\Classes\.pyi', 'Software\Classes\.ipynb')
        $assocSnapshot = { @($assocSub | ForEach-Object { Registry-Snapshot $_ }) -join "`n" }
        $shortcut = Join-Path $env:APPDATA 'Microsoft\Windows\Start Menu\Programs\RustPy.lnk'
        $blockers = @()
        if ((& $related).Count) { $blockers += "a RustPy MSI is already installed" }
        foreach ($k in $owned) { if (Test-Path -LiteralPath $k) { $blockers += "$k exists" } }
        if (Test-Path -LiteralPath $shortcut) { $blockers += "$shortcut exists" }
        $assocBlockers = @($assoc | Where-Object { Test-Path -LiteralPath $_ } | ForEach-Object { "$_ exists" })
        $installDir = Join-Path $tmp 'msi-install\RustPy'
        $manual = "msiexec /i `"$msiPath`" /qn INSTALLFOLDER=`"$installDir`""
        $manualNoAssoc = "$manual ASSOCIATE_FILES=0 CONTEXT_MENU=0"
        $exeInstalled = Join-Path $installDir 'rustpy.exe'
        # Installs with extra msiexec properties; checks what every install must create.
        $install = {
            param([string[]]$props, [string]$log, [string]$what)
            $script:msiInstalled = $true
            $inst = Start-Process msiexec.exe -ArgumentList (@('/i', "`"$msiPath`"", '/qn', '/norestart', "INSTALLFOLDER=`"$installDir`"") + $props + @('/l*v', "`"$msiDir\$log`"")) -Wait -PassThru
            Check ($inst.ExitCode -eq 0) "msiexec /i $($props -join ' ') ($what; per user, silent, into the temporary folder): exit $($inst.ExitCode)"
            Check (Test-Path -LiteralPath $exeInstalled) "rustpy.exe installed at $exeInstalled"
            Check ((& $related).Count -eq 1) "the product is registered with Windows Installer"
            Check ((& $featureState 'Core') -eq 3) "feature Core is installed (state $(& $featureState 'Core'))"
            $kind = (Get-ItemProperty -LiteralPath 'HKCU:\Software\RustPy' -ErrorAction SilentlyContinue).InstallKind
            Check ($kind -eq 'msi') "HKCU\Software\RustPy InstallKind = $kind"
            Check (Test-Path -LiteralPath $shortcut) "Start menu shortcut created"
            $openWith = (Get-Item -LiteralPath "$classes\Applications\rustpy.exe\shell\open\command" -ErrorAction SilentlyContinue).GetValue('')
            Check ("$openWith".ToLowerInvariant().Contains((Norm $exeInstalled))) "RustPy is registered for 'Open with' lists: $openWith"
            $pathNow = $envKey.GetValue('Path', $null, [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)
            Check ("$pathNow".ToLowerInvariant().Contains((Norm $installDir))) "the install folder was appended to your user PATH (AddToPath state $(& $featureState 'AddToPath'))"
        }
        # Uninstalls and checks that everything is gone; restores PATH and leftovers.
        $uninstall = {
            param([string]$log)
            $un = Start-Process msiexec.exe -ArgumentList @('/x', "`"$msiPath`"", '/qn', '/norestart', '/l*v', "`"$msiDir\$log`"") -Wait -PassThru
            Check ($un.ExitCode -eq 0) "msiexec /x: exit $($un.ExitCode)"
            $script:msiInstalled = ($un.ExitCode -ne 0)
            Check ((& $related).Count -eq 0) "the product is no longer registered"
            $leftKeys = @($owned | Where-Object { Test-Path -LiteralPath $_ })
            Check (-not $leftKeys.Count) "all RustPy registry keys removed ($($leftKeys -join ', '))"
            # Antivirus scanning the freshly installed exe can hold the folder briefly.
            for ($i = 0; $i -lt 20 -and (Test-Path -LiteralPath $installDir); $i++) { Start-Sleep -Milliseconds 500 }
            $leftFiles = @(Get-ChildItem -LiteralPath $installDir -Recurse -Force -ErrorAction SilentlyContinue | ForEach-Object { $_.FullName.Substring($installDir.Length) })
            Check (-not (Test-Path -LiteralPath $exeInstalled) -and -not (Test-Path -LiteralPath $installDir)) "install folder removed $(if (Test-Path -LiteralPath $installDir) { "(still there after 10 s; contents: $(if ($leftFiles.Count) { $leftFiles -join ', ' } else { 'empty' }))" })"
            Check (-not (Test-Path -LiteralPath $shortcut)) "Start menu shortcut removed"
            $pathAfter = $envKey.GetValue('Path', $null, [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)
            $pathRestored = ("$pathAfter" -eq "$pathBefore")
            $segments = { param($v) @("$v" -split ';' | Where-Object { $_ }) -join ';' }
            $sameEntries = ((& $segments $pathAfter) -eq (& $segments $pathBefore))
            Check $sameEntries "your user PATH has the same entries as before $(if (-not $pathRestored) { "(text differs: before '$pathBefore', after '$pathAfter')" })"
            if (-not $pathRestored) {
                if ($null -eq $pathBefore) { $envKey.DeleteValue('Path', $false) } else { $envKey.SetValue('Path', $pathBefore, $pathKind) }
                Note "  restored your user PATH to its previous value"
            }
            foreach ($k in $leftKeys) { Remove-Item -LiteralPath $k -Recurse -Force -ErrorAction SilentlyContinue; Note "  removed leftover $k" }
            if ($programsExisted -and -not (Test-Path -LiteralPath $programs)) { New-Item -ItemType Directory -Force $programs | Out-Null; Note "  re-created $programs" }
        }
        if ($SkipMsiInstall) {
            Skip "MSI install/uninstall (-SkipMsiInstall). Manual test: $manualNoAssoc   then   msiexec /x `"$msiPath`" /qn"
        } elseif ($blockers.Count) {
            Skip "MSI install/uninstall: would touch your existing setup ($($blockers -join '; ')). Manual test: $manualNoAssoc   then   msiexec /x `"$msiPath`" /qn"
        } else {
            $envKey = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey('Environment', $true)
            $pathBefore = $envKey.GetValue('Path', $null, [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)
            $pathKind = if ($null -ne $pathBefore) { $envKey.GetValueKind('Path') } else { $null }
            $programs = Join-Path $env:LOCALAPPDATA 'Programs'
            $programsExisted = Test-Path -LiteralPath $programs

            # (a) Associations and context menu deselected: your .py/.pyi/.ipynb keys
            # (default value and every subkey) must stay exactly as they are.
            $assocBefore = & $assocSnapshot
            & $install @('ASSOCIATE_FILES=0', 'CONTEXT_MENU=0') 'install-no-assoc.log' 'file associations and context menu off'
            # A feature disabled by its Level 0 condition reports -1 (unknown), deselected 2 (absent).
            Check (@(-1, 2) -contains (& $featureState 'FileAssociations') -and @(-1, 2) -contains (& $featureState 'ContextMenu') -and (& $featureState 'AddToPath') -eq 3) "feature states: FileAssociations $(& $featureState 'FileAssociations'), ContextMenu $(& $featureState 'ContextMenu') (-1 disabled / 2 absent), AddToPath $(& $featureState 'AddToPath') (3 = local)"
            $assocDuring = & $assocSnapshot
            Check ($assocDuring -ceq $assocBefore) "HKCU .py/.pyi/.ipynb keys untouched by the install (default values and subkeys compared)"
            if ($assocDuring -cne $assocBefore) { Note "  before: $($assocBefore -replace "`n", ' ; ')"; Note "  after:  $($assocDuring -replace "`n", ' ; ')" }
            $presentProg = @($progIds | Where-Object { Test-Path -LiteralPath $_ })
            Check (-not $presentProg.Count) "no RustPy ProgId registered ($($presentProg -join ', '))"
            $presentMenu = @($menuKeys | Where-Object { Test-Path -LiteralPath $_ })
            Check (-not $presentMenu.Count) "no Explorer context menu entry added ($($presentMenu -join ', '))"
            & $uninstall 'uninstall-no-assoc.log'
            Check ((& $assocSnapshot) -ceq $assocBefore) "HKCU .py/.pyi/.ipynb keys identical to before after the uninstall"

            # (b) Default install with every feature: only when no per-user association exists.
            if ($assocBlockers.Count) {
                Skip "MSI install with file associations: would change your associations ($($assocBlockers -join '; ')). Manual test: $manual   then   msiexec /x `"$msiPath`" /qn"
            } elseif (-not $msiInstalled) {
                & $install @() 'install.log' 'all features'
                Check ((& $featureState 'FileAssociations') -eq 3 -and (& $featureState 'ContextMenu') -eq 3) "feature states: FileAssociations $(& $featureState 'FileAssociations'), ContextMenu $(& $featureState 'ContextMenu') (3 = local)"
                $pyProg = (Get-Item -LiteralPath "$classes\.py" -ErrorAction SilentlyContinue).GetValue('')
                Check ($pyProg -eq 'RustPy.Python') ".py is associated with RustPy.Python ($pyProg)"
                $openCmd = (Get-Item -LiteralPath "$classes\RustPy.Python\shell\open\command" -ErrorAction SilentlyContinue).GetValue('')
                Check ("$openCmd".ToLowerInvariant().Contains((Norm $exeInstalled))) ".py open command runs the installed rustpy.exe: $openCmd"
                $ctx = (Get-Item -LiteralPath "$classes\*\shell\RustPy\command" -ErrorAction SilentlyContinue).GetValue('')
                Check ("$ctx".ToLowerInvariant().Contains((Norm $exeInstalled))) "Explorer context menu 'Open with RustPy': $ctx"
                & $uninstall 'uninstall.log'
                $leftAssoc = @($assoc | Where-Object { Test-Path -LiteralPath $_ })
                Check (-not $leftAssoc.Count) "the .py/.pyi/.ipynb associations removed ($($leftAssoc -join ', '))"
                foreach ($k in $leftAssoc) { Remove-Item -LiteralPath $k -Recurse -Force -ErrorAction SilentlyContinue; Note "  removed leftover $k" }
            }
            $envKey.Close()
        }
        foreach ($o in @($installer)) { if ($o) { try { [void][System.Runtime.InteropServices.Marshal]::FinalReleaseComObject($o) } catch {} } }
        $installer = $null
    }
}
Note ""
} catch {
    Note "FAIL  the script stopped unexpectedly: $_ $($_.InvocationInfo.PositionMessage)"
    $fails++
} finally {
    if ($msiInstalled -and $msiPath) {
        Note "cleaning up: uninstalling the test MSI"
        Start-Process msiexec.exe -ArgumentList @('/x', "`"$msiPath`"", '/qn', '/norestart') -Wait | Out-Null
    }
    $fullReport | ConvertTo-Json -Depth 12 | Out-File -Encoding utf8 (Join-Path $here 'windows-acceptance-2-report.json')
    if ($KeepTemp) { Note "kept $tmp" } else {
        Start-Sleep -Seconds 1
        Remove-Item -LiteralPath $tmp -Recurse -Force -ErrorAction SilentlyContinue
        if (Test-Path -LiteralPath $tmp) {
            Start-Sleep -Seconds 3
            Remove-Item -LiteralPath $tmp -Recurse -Force -ErrorAction SilentlyContinue
        }
        Note "temporary folder removed: $(-not (Test-Path -LiteralPath $tmp))"
    }
}

Note ("RESULT: " + $(if ($fails) { "$fails check(s) FAILED" } else { "all checks passed" }) + $(if ($skips) { ", $skips skipped" } else { '' }))
$summary | Out-File -Encoding utf8 (Join-Path $here 'windows-acceptance-2-summary.txt')
Write-Host "`nSaved windows-acceptance-2-summary.txt and windows-acceptance-2-report.json in $here"
exit $fails

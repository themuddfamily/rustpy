<# Native, silent package acceptance on a disposable GitHub-hosted Windows runner.
   Exercises portable CLI, fresh install, same-version reinstall/repair, optional
   integrations and uninstall. It does not launch the GUI or claim an older-version
   upgrade, accessibility, browser-engine or interactive installer acceptance. #>
#Requires -Version 7.2
[CmdletBinding()]
param(
    [Parameter(Mandatory=$true)][string]$Target,
    [string]$ArtifactsDir = "",
    [string]$Version = "",
    [string]$ReportDir = ""
)
$ErrorActionPreference = 'Stop'
if (-not $IsWindows -or $env:GITHUB_ACTIONS -ne 'true' -or $env:RUNNER_ENVIRONMENT -ne 'github-hosted') {
    throw 'Package acceptance requires a disposable GitHub-hosted Windows runner'
}
$Root = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$ZipArch = if ($Target -eq 'x86_64-pc-windows-msvc') { 'x86_64' } elseif ($Target -eq 'aarch64-pc-windows-msvc') { 'arm64' } else { throw 'Unsupported Windows target' }
$MsiArch = if ($ZipArch -eq 'x86_64') { 'x64' } else { 'arm64' }
if (-not $Version) {
    $Section = ''
    foreach ($Line in Get-Content -LiteralPath (Join-Path $Root 'Cargo.toml')) {
        if ($Line -match '^\s*\[([^\]]+)\]') { $Section = $Matches[1] }
        elseif ($Section -eq 'workspace.package' -and $Line -match '^\s*version\s*=\s*"([^"]+)"') { $Version = $Matches[1]; break }
    }
}
if (-not $Version) { throw 'Cannot read the package version' }
if ($Version -notmatch '^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?$') { throw 'Invalid package version' }
if (-not $ArtifactsDir) {
    $BuildRoot = if ($env:CARGO_TARGET_DIR) { [IO.Path]::GetFullPath([IO.Path]::Combine($Root, $env:CARGO_TARGET_DIR)) } else { Join-Path $Root 'target' }
    $ArtifactsDir = Join-Path $BuildRoot 'windows'
}
$ArtifactsDir = (Resolve-Path -LiteralPath $ArtifactsDir).Path
$Portable = Join-Path $ArtifactsDir "rustpy-$Version-windows-$ZipArch-portable.zip"
$Setup = Join-Path $ArtifactsDir "rustpy-$Version-windows-$ZipArch-setup.exe"
$Msi = Join-Path $ArtifactsDir "rustpy-$Version-windows-$MsiArch.msi"
foreach ($Path in @($Portable, $Setup, $Msi)) {
    if (-not (Test-Path -LiteralPath $Path -PathType Leaf) -or (Get-Item -LiteralPath $Path).Length -eq 0) { throw "Missing package: $Path" }
}

$Current = [Diagnostics.Process]::GetCurrentProcess()
$Current.PriorityClass = 'BelowNormal'
$Allowed = $Current.ProcessorAffinity.ToInt64()
$Selected = [long]0
$Count = 0
for ($Bit = 0; $Bit -lt 63 -and $Count -lt 2; $Bit++) {
    $Mask = [long]1 -shl $Bit
    if ($Allowed -band $Mask) { $Selected = $Selected -bor $Mask; $Count++ }
}
if (-not $Selected) { throw 'No CPUs available for package acceptance' }
$Current.ProcessorAffinity = [IntPtr]$Selected
$TaskRoot = Join-Path $env:RUNNER_TEMP 'rustpy-native-release'
New-Item -ItemType Directory -Force -Path $TaskRoot | Out-Null
$TestRoot = Join-Path $TaskRoot ("acceptance-$ZipArch-" + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $TestRoot | Out-Null
if (-not $ReportDir) { $ReportDir = Join-Path $TestRoot 'report' }
New-Item -ItemType Directory -Force -Path $ReportDir | Out-Null
$ReportDir = [IO.Path]::GetFullPath($ReportDir)
if ($env:GITHUB_ENV) { "RUSTPY_PACKAGE_ACCEPTANCE_DIR=$ReportDir" >> $env:GITHUB_ENV }
$Steps = [Collections.Generic.List[object]]::new()
$Snapshots = [Collections.Generic.List[object]]::new()
$SavedValues = [Collections.Generic.List[object]]::new()
$SavedEnvironment = @{}
$MsiInstalled = $false
$InnoInstalled = $false
$InnoDir = Join-Path $TestRoot 'inno install with spaces'
$MsiCoreDir = Join-Path $TestRoot 'msi core install with spaces'
$MsiFullDir = Join-Path $TestRoot 'msi full install with spaces'
$PortableDir = Join-Path $TestRoot 'portable with spaces'
$Failure = $null
$CleanupFailures = [Collections.Generic.List[string]]::new()

function Invoke-Checked {
    param([string]$Name, [string]$Program, [string[]]$Arguments, [int]$TimeoutSeconds = 240, [int[]]$AllowedCodes = @(0))
    $Info = [Diagnostics.ProcessStartInfo]::new()
    $Info.FileName = $Program
    $Info.UseShellExecute = $false
    $Info.CreateNoWindow = $true
    $Info.RedirectStandardOutput = $true
    $Info.RedirectStandardError = $true
    foreach ($Argument in $Arguments) { $Info.ArgumentList.Add($Argument) }
    $Child = [Diagnostics.Process]::new()
    $Child.StartInfo = $Info
    try {
        if (-not $Child.Start()) { throw "Cannot start $Name" }
        $Stdout = $Child.StandardOutput.ReadToEndAsync()
        $Stderr = $Child.StandardError.ReadToEndAsync()
        if (-not $Child.WaitForExit($TimeoutSeconds * 1000)) {
            $Child.Kill($true)
            $null = $Child.WaitForExit(10000)
            throw "$Name exceeded $TimeoutSeconds seconds"
        }
        if (-not $Stdout.Wait(5000) -or -not $Stderr.Wait(5000)) { throw "$Name left output pipes open" }
        $Text = $Stdout.Result
        ($Text + $Stderr.Result) | Set-Content -LiteralPath (Join-Path $ReportDir "$Name-output.log") -Encoding utf8
        $Code = $Child.ExitCode
        $Steps.Add(@{ name=$Name; exit_code=$Code })
        if ($Code -notin $AllowedCodes) { throw "$Name exited $Code; see its acceptance log" }
        return $Text
    } finally { $Child.Dispose() }
}

function Assert-True([bool]$Condition, [string]$Message) {
    if (-not $Condition) { throw $Message }
}

function Get-DefaultRegistryValue([string]$Path) {
    if (-not (Test-Path -LiteralPath $Path)) { return $null }
    return (Get-Item -LiteralPath $Path).GetValue('', $null, [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)
}

function Save-RegistryValue([string]$Path, [string]$Name) {
    $Exists = Test-Path -LiteralPath $Path
    $Entry = @{ path=$Path; name=$Name; exists=$false; key_exists=$Exists }
    if ($Exists) {
        $Key = Get-Item -LiteralPath $Path
        if ($Name -in $Key.GetValueNames()) {
            $Entry.exists = $true
            $Entry.value = $Key.GetValue($Name, $null, [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)
            $Entry.kind = $Key.GetValueKind($Name).ToString()
        }
    }
    $SavedValues.Add($Entry)
}

function Save-RegistryTree([string]$Relative) {
    $Path = "Registry::HKEY_CURRENT_USER\$Relative"
    $Exists = Test-Path -LiteralPath $Path
    $Backup = Join-Path $TestRoot ("registry-$($Snapshots.Count).reg")
    if ($Exists) { $null = Invoke-Checked "registry-export-$($Snapshots.Count)" 'reg.exe' @('export', "HKCU\$Relative", $Backup, '/y') }
    $Snapshots.Add(@{ path=$Path; backup=$Backup; exists=$Exists })
}

function Restore-Registry {
    foreach ($Entry in $Snapshots) {
        if (Test-Path -LiteralPath $Entry.path) { Remove-Item -LiteralPath $Entry.path -Recurse -Force }
        if ($Entry.exists) { $null = Invoke-Checked ("registry-restore-" + [IO.Path]::GetFileNameWithoutExtension($Entry.backup)) 'reg.exe' @('import', $Entry.backup) }
    }
    foreach ($Entry in $SavedValues) {
        if ($Entry.exists) {
            New-Item -Path $Entry.path -Force | Out-Null
            New-ItemProperty -LiteralPath $Entry.path -Name $Entry.name -Value $Entry.value -PropertyType $Entry.kind -Force | Out-Null
        } elseif (Test-Path -LiteralPath $Entry.path) {
            Remove-ItemProperty -LiteralPath $Entry.path -Name $Entry.name -ErrorAction SilentlyContinue
            if (-not $Entry.key_exists) {
                $Key = Get-Item -LiteralPath $Entry.path
                if ($Key.ValueCount -eq 0 -and $Key.SubKeyCount -eq 0) { Remove-Item -LiteralPath $Entry.path }
            }
        }
    }
}

function Get-UserPath {
    $Key = Get-Item -LiteralPath 'Registry::HKEY_CURRENT_USER\Environment' -ErrorAction SilentlyContinue
    if ($null -eq $Key) { return '' }
    return [string]$Key.GetValue('Path', '', [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)
}

function Assert-PathEntry([string]$InstallDir, [bool]$Present) {
    $PathEntries = @((Get-UserPath) -split ';' | Where-Object { $_.Trim().TrimEnd('\') -ieq $InstallDir.TrimEnd('\') })
    Assert-True (($Present -and $PathEntries.Count -eq 1) -or (-not $Present -and $PathEntries.Count -eq 0)) "Unexpected PATH registration for $InstallDir"
}

function Assert-Command([string]$Path, [string]$Exe, [string]$Placeholder = '%1') {
    $Expected = '"' + $Exe + '" "' + $Placeholder + '"'
    Assert-True ((Get-DefaultRegistryValue $Path) -ieq $Expected) "Unquoted or incorrect launch command: $Path"
}

function Assert-Archive([string]$Path, [string[]]$Required) {
    $Archive = [IO.Compression.ZipFile]::OpenRead($Path)
    try {
        $Entries = @{}
        foreach ($Entry in $Archive.Entries) {
            $Name = $Entry.FullName.Replace('\', '/')
            Assert-True (-not ($Name.StartsWith('/') -or $Name -match '(^|/)\.\.(/|$)' -or $Name -match '^[A-Za-z]:')) "Unsafe ZIP member: $Name"
            Assert-True (-not $Entries.ContainsKey($Name)) "Duplicate ZIP member: $Name"
            $Entries[$Name] = $Entry
        }
        foreach ($Name in $Required) {
            Assert-True ($Entries.ContainsKey($Name) -and $Entries[$Name].Length -gt 0) "Missing archive member $Name in $Path"
        }
    } finally { $Archive.Dispose() }
}

function Assert-NativeEvidence([string]$Directory) {
    $Zip = Join-Path $Directory 'Native-Licenses.zip'
    Assert-Archive $Zip @('Native-Licenses/COMPLETE', 'Native-Licenses/runtime-manifest.json', 'Native-Licenses/native-crates.json', 'Native-Licenses/NOTICE-native-runtime-dependencies.txt', 'Native-Licenses/cargo-covered-sources.tar.gz')
    $Archive = [IO.Compression.ZipFile]::OpenRead($Zip)
    try {
        Assert-True (@($Archive.Entries | Where-Object { $_.FullName -like 'Native-Licenses/sources/*.crate' -and $_.Length -gt 0 }).Count -gt 0) 'Native source archives are missing'
        $Entry = $Archive.GetEntry('Native-Licenses/runtime-manifest.json')
        Assert-True ($Entry.Length -lt 16MB) 'Runtime manifest is unexpectedly large'
        $Reader = [IO.StreamReader]::new($Entry.Open())
        try { $Manifest = $Reader.ReadToEnd() | ConvertFrom-Json } finally { $Reader.Dispose() }
        $Hash = (Get-FileHash -LiteralPath (Join-Path $Directory 'rustpy.exe') -Algorithm SHA256).Hash.ToLowerInvariant()
        Assert-True ($Manifest.binary_sha256 -ceq $Hash) 'Native evidence does not describe the installed executable'
        Assert-True ($null -ne $Manifest.system_static_components -and @($Manifest.system_static_components).Count -gt 0) 'Windows static runtime evidence is missing'
    } finally { $Archive.Dispose() }
}

function Assert-Installed([string]$Directory, [string]$Label) {
    foreach ($Name in @('rustpy.exe', 'LICENSE', 'THIRD-PARTY-NOTICES.txt', 'Native-Licenses.zip')) {
        $Path = Join-Path $Directory $Name
        Assert-True ((Test-Path -LiteralPath $Path -PathType Leaf) -and (Get-Item -LiteralPath $Path).Length -gt 0) "Missing installed file $Name"
    }
    Assert-NativeEvidence $Directory
    $Exe = Join-Path $Directory 'rustpy.exe'
    $Text = Invoke-Checked "$Label-version" $Exe @('--version') 60
    Assert-True ($Text.Trim() -ceq "RustPy $Version") "Incorrect installed version: $Text"
    $Help = Invoke-Checked "$Label-help" $Exe @('--help') 60
    Assert-True ($Help -match 'Usage:') 'Installed CLI help did not render'
}

function Assert-SettingsPreserved {
    Assert-True ((Get-Content -Raw -LiteralPath $SettingsMarker) -ceq 'preserve this isolated configuration') 'Uninstall changed the isolated configuration'
}

Add-Type -AssemblyName System.IO.Compression.FileSystem
$InnoAppId = '{8C1B9B9E-4B4E-4F7C-9D53-2A2B5E3C7F10}_is1'
$ExistingInstallKeys = @('Registry::HKEY_CURRENT_USER\Software\RustPy', "Registry::HKEY_CURRENT_USER\Software\Microsoft\Windows\CurrentVersion\Uninstall\$InnoAppId", 'Registry::HKEY_CURRENT_USER\Software\Microsoft\Windows\CurrentVersion\App Paths\rustpy.exe', 'Registry::HKEY_LOCAL_MACHINE\Software\RustPy')
$Programs = [Environment]::GetFolderPath('Programs')
$DefaultInstallDir = Join-Path ([Environment]::GetFolderPath('LocalApplicationData')) 'Programs\RustPy'
foreach ($Path in $ExistingInstallKeys + @((Join-Path $Programs 'RustPy'), (Join-Path $Programs 'RustPy.lnk'), $DefaultInstallDir)) {
    if (Test-Path -LiteralPath $Path) { throw "Refusing to overwrite an existing RustPy installation: $Path" }
}

try {
    foreach ($Name in @('RUSTPY_CONFIG_DIR', 'RUSTPY_SYSTEM_DIR', 'RUSTPY_LOG_DIR', 'XDG_CONFIG_HOME', 'XDG_CACHE_HOME', 'XDG_DATA_HOME', 'XDG_STATE_HOME', 'TMPDIR', 'TMP', 'TEMP', 'CARGO_BUILD_JOBS', 'RAYON_NUM_THREADS', 'RUST_TEST_THREADS')) {
        $SavedEnvironment[$Name] = [Environment]::GetEnvironmentVariable($Name, 'Process')
        $Value = if ($Name -in @('CARGO_BUILD_JOBS', 'RAYON_NUM_THREADS', 'RUST_TEST_THREADS')) { '2' } else { Join-Path $TestRoot ($Name.ToLowerInvariant()) }
        if ($Value -ne '2') { New-Item -ItemType Directory -Force -Path $Value | Out-Null }
        [Environment]::SetEnvironmentVariable($Name, $Value, 'Process')
    }
    $SettingsMarker = Join-Path $env:RUSTPY_CONFIG_DIR 'acceptance-sentinel.txt'
    [IO.File]::WriteAllText($SettingsMarker, 'preserve this isolated configuration')
    foreach ($Relative in @('Software\RustPy', 'Software\Classes\Applications\rustpy.exe', 'Software\Microsoft\Windows\CurrentVersion\App Paths\rustpy.exe', 'Software\Classes\.py', 'Software\Classes\.pyw', 'Software\Classes\.pyi', 'Software\Classes\.ipynb', 'Software\Classes\RustPy.py', 'Software\Classes\RustPy.pyw', 'Software\Classes\RustPy.pyi', 'Software\Classes\RustPy.ipynb', 'Software\Classes\RustPy.Python', 'Software\Classes\RustPy.PythonStub', 'Software\Classes\RustPy.Notebook', 'Software\Classes\*\shell\RustPy.Edit', 'Software\Classes\*\shell\RustPy', 'Software\Classes\Directory\shell\RustPy', 'Software\Classes\Directory\Background\shell\RustPy')) { Save-RegistryTree $Relative }
    Save-RegistryValue 'Registry::HKEY_CURRENT_USER\Environment' 'Path'
    Save-RegistryValue 'Registry::HKEY_CURRENT_USER\Software\RegisteredApplications' 'RustPy'
    $OldDefaults = @{}
    foreach ($Suffix in @('py', 'pyw', 'pyi', 'ipynb')) { $OldDefaults[$Suffix] = Get-DefaultRegistryValue "Registry::HKEY_CURRENT_USER\Software\Classes\.$Suffix" }

    Assert-Archive $Portable @('rustpy.exe', 'rustpy-portable.cmd', 'LICENSE', 'THIRD-PARTY-NOTICES.txt', 'Native-Licenses.zip')
    [IO.Compression.ZipFile]::ExtractToDirectory($Portable, $PortableDir)
    Assert-Installed $PortableDir 'portable'
    $Steps.Add(@{ name='portable archive and isolated CLI'; passed=$true })

    $InnoBase = @('/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART', '/CURRENTUSER', "/DIR=$InnoDir")
    $InnoInstalled = $true
    $null = Invoke-Checked 'inno-install' $Setup ($InnoBase + @('/TASKS=', "/LOG=$(Join-Path $ReportDir 'inno-install.log')"))
    Assert-Installed $InnoDir 'inno-fresh'
    Assert-True (Test-Path -LiteralPath (Join-Path $Programs 'RustPy\RustPy.lnk')) 'Inno Start menu launcher missing'
    Assert-PathEntry $InnoDir $false
    $Tasks = 'addtopath,associate,associate\py,associate\pyw,associate\pyi,associate\ipynb,contextmenu,contextmenu\folder,contextmenu\file'
    $null = Invoke-Checked 'inno-reinstall' $Setup ($InnoBase + @("/TASKS=$Tasks", "/LOG=$(Join-Path $ReportDir 'inno-reinstall.log')"))
    Assert-Installed $InnoDir 'inno-reinstalled'
    Assert-PathEntry $InnoDir $true
    foreach ($Suffix in @('py', 'pyw', 'pyi', 'ipynb')) {
        Assert-Command "Registry::HKEY_CURRENT_USER\Software\Classes\RustPy.$Suffix\shell\open\command" (Join-Path $InnoDir 'rustpy.exe')
        Assert-True ((Get-DefaultRegistryValue "Registry::HKEY_CURRENT_USER\Software\Classes\.$Suffix") -ceq $OldDefaults[$Suffix]) 'Inno changed an existing default file handler'
        $OpenWith = Get-Item -LiteralPath "Registry::HKEY_CURRENT_USER\Software\Classes\.$Suffix\OpenWithProgids"
        Assert-True ("RustPy.$Suffix" -in $OpenWith.GetValueNames()) 'Inno Open with registration missing'
    }
    Assert-Command 'Registry::HKEY_CURRENT_USER\Software\Classes\Directory\shell\RustPy\command' (Join-Path $InnoDir 'rustpy.exe')
    Assert-Command 'Registry::HKEY_CURRENT_USER\Software\Classes\Directory\Background\shell\RustPy\command' (Join-Path $InnoDir 'rustpy.exe') '%V'
    Assert-Command 'Registry::HKEY_CURRENT_USER\Software\Classes\*\shell\RustPy.Edit\command' (Join-Path $InnoDir 'rustpy.exe')
    $null = Invoke-Checked 'inno-uninstall' (Join-Path $InnoDir 'unins000.exe') @('/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART', "/LOG=$(Join-Path $ReportDir 'inno-uninstall.log')")
    $InnoInstalled = $false
    Assert-True (-not (Test-Path -LiteralPath (Join-Path $InnoDir 'rustpy.exe'))) 'Inno uninstall left the executable'
    Assert-True (-not (Test-Path -LiteralPath (Join-Path $Programs 'RustPy\RustPy.lnk'))) 'Inno uninstall left its launcher'
    Assert-PathEntry $InnoDir $false
    foreach ($Suffix in @('py', 'pyw', 'pyi', 'ipynb')) { Assert-True (-not (Test-Path -LiteralPath "Registry::HKEY_CURRENT_USER\Software\Classes\RustPy.$Suffix")) 'Inno uninstall left a ProgID' }
    Assert-SettingsPreserved
    Restore-Registry

    $Installer = New-Object -ComObject WindowsInstaller.Installer
    # COM late binding needs plain values, not PowerShell-wrapped arguments.
    $Database = $Installer.GetType().InvokeMember('OpenDatabase', 'InvokeMethod', $null, $Installer, [object[]]@([string]$Msi, [int]0))
    $Query = "SELECT ``Value`` FROM ``Property`` WHERE ``Property``='ProductCode'"
    $View = $Database.GetType().InvokeMember('OpenView', 'InvokeMethod', $null, $Database, [object[]]@([string]$Query))
    $null = $View.GetType().InvokeMember('Execute', 'InvokeMethod', $null, $View, $null)
    $Record = $View.GetType().InvokeMember('Fetch', 'InvokeMethod', $null, $View, $null)
    Assert-True ($null -ne $Record) 'MSI ProductCode property is missing'
    $ProductCode = [string]$Record.GetType().InvokeMember('StringData', 'GetProperty', $null, $Record, [object[]]@([int]1))
    $null = $View.GetType().InvokeMember('Close', 'InvokeMethod', $null, $View, $null)
    Assert-True ($ProductCode -match '^\{[0-9A-Fa-f-]{36}\}$') 'Cannot read MSI product code'
    foreach ($Object in @($Record, $View, $Database, $Installer)) { $null = [Runtime.InteropServices.Marshal]::FinalReleaseComObject($Object) }
    $MsiUninstallKey = "Registry::HKEY_CURRENT_USER\Software\Microsoft\Windows\CurrentVersion\Uninstall\$ProductCode"
    Assert-True (-not (Test-Path -LiteralPath $MsiUninstallKey)) 'This MSI product is already installed'
    foreach ($Mode in @('core', 'full')) {
        $Directory = if ($Mode -eq 'core') { $MsiCoreDir } else { $MsiFullDir }
        $Enabled = if ($Mode -eq 'core') { '0' } else { '1' }
        $Features = if ($Mode -eq 'core') { 'Core' } else { 'Core,FileAssociations,ContextMenu,AddToPath' }
        $Properties = @('ALLUSERS=', 'MSIINSTALLPERUSER=1', "INSTALLFOLDER=$Directory", "ADDLOCAL=$Features", "ASSOCIATE_FILES=$Enabled", "CONTEXT_MENU=$Enabled", "ADD_TO_PATH=$Enabled")
        $MsiInstalled = $true
        $null = Invoke-Checked "msi-$Mode-install" 'msiexec.exe' (@('/i', $Msi, '/qn', '/norestart', '/L*V', (Join-Path $ReportDir "msi-$Mode-install.log")) + $Properties) 360 @(0, 3010)
        Assert-Installed $Directory "msi-$Mode-fresh"
        Assert-True (Test-Path -LiteralPath (Join-Path $Programs 'RustPy.lnk')) 'MSI Start menu launcher missing'
        Assert-True ((Get-ItemProperty -LiteralPath 'Registry::HKEY_CURRENT_USER\Software\RustPy').InstallFolder.TrimEnd('\') -ieq $Directory.TrimEnd('\')) 'MSI did not remember the custom installation directory'
        Assert-PathEntry $Directory ($Mode -eq 'full')
        if ($Mode -eq 'core') {
            Assert-True (-not (Test-Path -LiteralPath 'Registry::HKEY_CURRENT_USER\Software\Classes\RustPy.Python')) 'Core-only MSI unexpectedly registered file associations'
            Assert-True (-not (Test-Path -LiteralPath 'Registry::HKEY_CURRENT_USER\Software\Classes\*\shell\RustPy')) 'Core-only MSI unexpectedly registered a context menu'
            Remove-Item -LiteralPath (Join-Path $Directory 'rustpy.exe')
            $null = Invoke-Checked 'msi-core-repair' 'msiexec.exe' @('/fa', $ProductCode, '/qn', '/norestart', '/L*V', (Join-Path $ReportDir 'msi-core-repair.log'), 'ASSOCIATE_FILES=0', 'CONTEXT_MENU=0', 'ADD_TO_PATH=0') 360 @(0, 3010)
            Assert-Installed $Directory 'msi-repaired'
            Assert-True (-not (Test-Path -LiteralPath (Join-Path $DefaultInstallDir 'rustpy.exe'))) 'MSI repair ignored the remembered custom directory'
        } else {
            foreach ($Pair in @(@('py', 'RustPy.Python'), @('pyi', 'RustPy.PythonStub'), @('ipynb', 'RustPy.Notebook'))) {
                Assert-True ((Get-DefaultRegistryValue "Registry::HKEY_CURRENT_USER\Software\Classes\.$($Pair[0])") -ceq $Pair[1]) 'MSI file association missing'
                Assert-Command "Registry::HKEY_CURRENT_USER\Software\Classes\$($Pair[1])\shell\open\command" (Join-Path $Directory 'rustpy.exe')
            }
            Assert-Command 'Registry::HKEY_CURRENT_USER\Software\Classes\*\shell\RustPy\command' (Join-Path $Directory 'rustpy.exe')
            Assert-Command 'Registry::HKEY_CURRENT_USER\Software\Classes\Directory\shell\RustPy\command' (Join-Path $Directory 'rustpy.exe')
            Assert-Command 'Registry::HKEY_CURRENT_USER\Software\Classes\Directory\Background\shell\RustPy\command' (Join-Path $Directory 'rustpy.exe') '%V'
        }
        $null = Invoke-Checked "msi-$Mode-uninstall" 'msiexec.exe' @('/x', $ProductCode, '/qn', '/norestart', '/L*V', (Join-Path $ReportDir "msi-$Mode-uninstall.log")) 360 @(0, 3010)
        $MsiInstalled = $false
        Assert-True (-not (Test-Path -LiteralPath (Join-Path $Directory 'rustpy.exe'))) 'MSI uninstall left the executable'
        Assert-True (-not (Test-Path -LiteralPath (Join-Path $Programs 'RustPy.lnk'))) 'MSI uninstall left its launcher'
        Assert-PathEntry $Directory $false
        Assert-True (-not (Test-Path -LiteralPath 'Registry::HKEY_CURRENT_USER\Software\Classes\RustPy.Python')) 'MSI uninstall left a ProgID'
        Assert-SettingsPreserved
        Restore-Registry
    }
} catch {
    $Failure = $_
} finally {
    if ($InnoInstalled -and (Test-Path -LiteralPath (Join-Path $InnoDir 'unins000.exe'))) {
        try { $null = Invoke-Checked 'inno-failure-cleanup' (Join-Path $InnoDir 'unins000.exe') @('/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART') } catch { $CleanupFailures.Add($_.ToString()) }
    }
    if ($MsiInstalled -and $ProductCode) {
        try { $null = Invoke-Checked 'msi-failure-cleanup' 'msiexec.exe' @('/x', $ProductCode, '/qn', '/norestart') 360 @(0, 3010, 1605) } catch { $CleanupFailures.Add($_.ToString()) }
    }
    try { Restore-Registry } catch { $CleanupFailures.Add($_.ToString()) }
    foreach ($Name in $SavedEnvironment.Keys) { [Environment]::SetEnvironmentVariable($Name, $SavedEnvironment[$Name], 'Process') }
    $Report = @{
        version=$Version; target=$Target; passed=($null -eq $Failure -and $CleanupFailures.Count -eq 0)
        steps=$Steps; cleanup_failures=$CleanupFailures
        limitations=@('No GUI or interactive installer was opened', 'Older-version upgrades are untested because no prior public release is available', 'Code signing, accessibility and embedded browser acceptance are separate checks')
    }
    if ($Failure) { $Report.failure = $Failure.ToString() }
    $Report | ConvertTo-Json -Depth 10 | Set-Content -LiteralPath (Join-Path $ReportDir 'acceptance.json') -Encoding utf8
}
if ($Failure) { throw $Failure }
if ($CleanupFailures.Count) { throw ($CleanupFailures -join "`n") }
Write-Host "Windows $Target package acceptance passed; evidence: $ReportDir"

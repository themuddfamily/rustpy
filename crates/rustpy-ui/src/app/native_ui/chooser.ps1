$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [System.Text.UTF8Encoding]::new($false)
Add-Type -AssemblyName System.Windows.Forms
$request = $env:RUSTPY_NATIVE_CHOOSER | ConvertFrom-Json
Remove-Item Env:RUSTPY_NATIVE_CHOOSER
$dialog = $null
try {
    if ($request.mode -eq 'directory') {
        $dialog = New-Object System.Windows.Forms.FolderBrowserDialog
        $dialog.Description = 'Open a project folder'
        $dialog.SelectedPath = $request.directory
        $dialog.ShowNewFolderButton = $false
    } elseif ($request.mode -eq 'save') {
        $dialog = New-Object System.Windows.Forms.SaveFileDialog
        $dialog.Title = 'Save a copy'
        $dialog.InitialDirectory = $request.directory
        $dialog.FileName = $request.name
        $dialog.OverwritePrompt = $true
        $dialog.CheckPathExists = $true
    } else {
        $dialog = New-Object System.Windows.Forms.OpenFileDialog
        $dialog.Title = 'Open a file'
        $dialog.InitialDirectory = $request.directory
        $dialog.Multiselect = $false
        $dialog.CheckFileExists = $true
    }
    if ($dialog.ShowDialog() -eq [System.Windows.Forms.DialogResult]::OK) {
        if ($request.mode -eq 'directory') { $path = $dialog.SelectedPath } else { $path = $dialog.FileName }
        @{ path = $path } | ConvertTo-Json -Compress
    } else {
        @{ path = $null } | ConvertTo-Json -Compress
    }
} finally {
    if ($null -ne $dialog) { $dialog.Dispose() }
}

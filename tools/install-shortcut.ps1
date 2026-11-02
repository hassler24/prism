param(
    [string]$Exe = (Join-Path $PSScriptRoot "..\target\release\prism.exe"),
    [string]$Name = "Prism"
)

$exe = (Resolve-Path -LiteralPath $Exe).Path
$desktop = [Environment]::GetFolderPath("Desktop")
$lnk = Join-Path $desktop "$Name.lnk"

$wsh = New-Object -ComObject WScript.Shell
$shortcut = $wsh.CreateShortcut($lnk)
$shortcut.TargetPath = $exe
$shortcut.WorkingDirectory = Split-Path -Parent $exe
$shortcut.IconLocation = "$exe,0"
$shortcut.Description = "Prism - instant file finder"
$shortcut.WindowStyle = 1
$shortcut.Save()

Write-Output $lnk

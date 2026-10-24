param(
    [switch]$SetDefault
)

$ErrorActionPreference = "Stop"

$source = Split-Path -Parent $MyInvocation.MyCommand.Path
$target = Join-Path $env:LOCALAPPDATA "Programs\Quire"
$exe = Join-Path $target "Quire.exe"

New-Item -ItemType Directory -Path $target -Force | Out-Null
Copy-Item -LiteralPath (Join-Path $source "Quire.exe") -Destination $exe -Force
Copy-Item -LiteralPath (Join-Path $source "README.txt") -Destination $target -Force
Copy-Item -LiteralPath (Join-Path $source "LICENSE.txt") -Destination $target -Force
$notice = Join-Path $source "NOTICE.txt"
if (Test-Path -LiteralPath $notice) {
    Copy-Item -LiteralPath $notice -Destination $target -Force
}

$classes = "HKCU:\Software\Classes"
$progId = "Quire.docx"

New-Item -Path "$classes\$progId\DefaultIcon" -Force | Out-Null
Set-ItemProperty -Path "$classes\$progId\DefaultIcon" -Name "(default)" -Value "`"$exe`",0"
New-Item -Path "$classes\$progId\shell\open\command" -Force | Out-Null
Set-ItemProperty -Path "$classes\$progId\shell\open\command" -Name "(default)" -Value "`"$exe`" `"%1`""

$extensions = @(".docx", ".rtf", ".odt", ".txt", ".xlsx", ".csv")
foreach ($extension in $extensions) {
    New-Item -Path "$classes\$extension\OpenWithProgids" -Force | Out-Null
    New-ItemProperty -Path "$classes\$extension\OpenWithProgids" -Name $progId -Value "" -PropertyType String -Force | Out-Null

    New-Item -Path "$classes\$extension\shell\OpenWithQuire\command" -Force | Out-Null
    Set-ItemProperty -Path "$classes\$extension\shell\OpenWithQuire" -Name "(default)" -Value "用 Quire 打开"
    Set-ItemProperty -Path "$classes\$extension\shell\OpenWithQuire" -Name "Icon" -Value "`"$exe`",0"
    Set-ItemProperty -Path "$classes\$extension\shell\OpenWithQuire\command" -Name "(default)" -Value "`"$exe`" `"%1`""
}

New-Item -Path "$classes\Applications\Quire.exe\shell\open\command" -Force | Out-Null
Set-ItemProperty -Path "$classes\Applications\Quire.exe\shell\open\command" -Name "(default)" -Value "`"$exe`" `"%1`""
Set-ItemProperty -Path "$classes\Applications\Quire.exe" -Name "FriendlyAppName" -Value "Quire"
New-Item -Path "$classes\Applications\Quire.exe\SupportedTypes" -Force | Out-Null
foreach ($extension in @(".docx", ".rtf", ".odt", ".txt", ".xlsx", ".csv")) {
    Set-ItemProperty -Path "$classes\Applications\Quire.exe\SupportedTypes" -Name $extension -Value ""
}

New-Item -Path "HKCU:\Software\Quire\Capabilities\FileAssociations" -Force | Out-Null
Set-ItemProperty -Path "HKCU:\Software\Quire\Capabilities" -Name "ApplicationName" -Value "Quire"
Set-ItemProperty -Path "HKCU:\Software\Quire\Capabilities" -Name "ApplicationDescription" -Value "Quire 文档编辑器"
Set-ItemProperty -Path "HKCU:\Software\Quire\Capabilities" -Name "ApplicationIcon" -Value "`"$exe`",0"
foreach ($extension in $extensions) {
    Set-ItemProperty -Path "HKCU:\Software\Quire\Capabilities\FileAssociations" -Name $extension -Value $progId
}
New-Item -Path "HKCU:\Software\RegisteredApplications" -Force | Out-Null
Set-ItemProperty -Path "HKCU:\Software\RegisteredApplications" -Name "Quire" -Value "Software\Quire\Capabilities"

$startMenu = Join-Path $env:APPDATA "Microsoft\Windows\Start Menu\Programs"
New-Item -ItemType Directory -Path $startMenu -Force | Out-Null
$shortcutPath = Join-Path $startMenu "Quire.lnk"
$shell = New-Object -ComObject WScript.Shell
$shortcut = $shell.CreateShortcut($shortcutPath)
$shortcut.TargetPath = $exe
$shortcut.WorkingDirectory = $target
$shortcut.IconLocation = "$exe,0"
$shortcut.Description = "Quire 文档编辑器"
$shortcut.Save()

Write-Host ""
Write-Host "Quire 已安装到: $target"
Write-Host "已注册 .docx 的“打开方式”和右键菜单。"

if ($SetDefault) {
    $previous = (Get-ItemProperty -Path "$classes\.docx" -Name "(default)" -ErrorAction SilentlyContinue)."(default)"
    if ($previous) {
        Set-ItemProperty -Path "HKCU:\Software\Quire" -Name "PreviousDocxDefault" -Value $previous
    }
    Set-ItemProperty -Path "$classes\.docx" -Name "(default)" -Value $progId
    Write-Host "已尝试把 .docx 默认程序设为 Quire。"
    Write-Host "如果系统已有 UserChoice，Windows 仍会使用原默认应用；请在设置中手动切换。"
} else {
    Write-Host ""
    Write-Host "Windows 10/11 不允许程序静默修改默认应用。请在弹出的设置页中："
    Write-Host "  应用 -> 默认应用 -> 按文件类型选择 .docx -> 选择 Quire"
}

Write-Host ""
Write-Host "也可以右键任意 .docx，选择“打开方式 -> Quire”。"
Start-Process "ms-settings:defaultapps?registeredAppUser=Quire"


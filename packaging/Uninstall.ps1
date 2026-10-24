$ErrorActionPreference = "Continue"

$target = Join-Path $env:LOCALAPPDATA "Programs\Quire"
$classes = "HKCU:\Software\Classes"
$progId = "Quire.docx"

Remove-Item -Path "$classes\$progId" -Recurse -Force -ErrorAction SilentlyContinue
foreach ($extension in @(".docx", ".rtf", ".odt", ".txt", ".xlsx", ".csv")) {
    Remove-Item -Path "$classes\$extension\shell\OpenWithQuire" -Recurse -Force -ErrorAction SilentlyContinue
    Remove-ItemProperty -Path "$classes\$extension\OpenWithProgids" -Name $progId -ErrorAction SilentlyContinue
}
Remove-Item -Path "$classes\Applications\Quire.exe" -Recurse -Force -ErrorAction SilentlyContinue
Remove-Item -Path "HKCU:\Software\Quire" -Recurse -Force -ErrorAction SilentlyContinue
Remove-ItemProperty -Path "HKCU:\Software\RegisteredApplications" -Name "Quire" -ErrorAction SilentlyContinue
Remove-ItemProperty -Path "$classes\.docx\OpenWithProgids" -Name $progId -ErrorAction SilentlyContinue

$previous = (Get-ItemProperty -Path "HKCU:\Software\Quire" -Name "PreviousDocxDefault" -ErrorAction SilentlyContinue)."PreviousDocxDefault"
$current = (Get-ItemProperty -Path "$classes\.docx" -Name "(default)" -ErrorAction SilentlyContinue)."(default)"
if ($current -eq $progId) {
    if ($previous) {
        Set-ItemProperty -Path "$classes\.docx" -Name "(default)" -Value $previous
    } else {
        Remove-ItemProperty -Path "$classes\.docx" -Name "(default)" -ErrorAction SilentlyContinue
    }
}

$shortcut = Join-Path $env:APPDATA "Microsoft\Windows\Start Menu\Programs\Quire.lnk"
Remove-Item -LiteralPath $shortcut -Force -ErrorAction SilentlyContinue
Remove-Item -LiteralPath $target -Recurse -Force -ErrorAction SilentlyContinue

Write-Host "Quire 的启动菜单快捷方式、打开方式注册和安装目录已移除。"
Write-Host "如果 .docx 的默认应用仍是 Quire，请在系统设置中改回其他应用。"


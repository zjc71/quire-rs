param(
    [string]$Version = "0.1.0"
)

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
$dist = Join-Path $root "dist"
$out = Join-Path $dist "Quire"

if (Test-Path -LiteralPath $out) {
    Remove-Item -LiteralPath $out -Recurse -Force
}
New-Item -ItemType Directory -Path $out -Force | Out-Null

# Static CRT: the packaged exe only depends on Windows system DLLs.
$env:RUSTFLAGS = "-C target-feature=+crt-static"

Write-Host "building release binaries..."
& cargo build --release -p quire-ui --bin quire-view --manifest-path (Join-Path $root "Cargo.toml")
if ($LASTEXITCODE -ne 0) { throw "quire-ui build failed" }
& cargo build --release -p quire-cli --manifest-path (Join-Path $root "Cargo.toml")
if ($LASTEXITCODE -ne 0) { throw "quire-cli build failed" }

$exe = Join-Path $root "target\release\quire-view.exe"
if (-not (Test-Path -LiteralPath $exe)) { throw "missing $exe" }

Copy-Item -LiteralPath $exe -Destination (Join-Path $out "Quire.exe")
Copy-Item -LiteralPath (Join-Path $root "LICENSE") -Destination (Join-Path $out "LICENSE.txt")
Copy-Item -LiteralPath (Join-Path $root "NOTICE") -Destination (Join-Path $out "NOTICE.txt")
Copy-Item -LiteralPath (Join-Path $root "packaging\README.txt") -Destination (Join-Path $out "README.txt")
Copy-Item -LiteralPath (Join-Path $root "packaging\Install.ps1") -Destination (Join-Path $out "Install.ps1")
Copy-Item -LiteralPath (Join-Path $root "packaging\Install.cmd") -Destination (Join-Path $out "Install.cmd")
Copy-Item -LiteralPath (Join-Path $root "packaging\Uninstall.ps1") -Destination (Join-Path $out "Uninstall.ps1")
Copy-Item -LiteralPath (Join-Path $root "packaging\Uninstall.cmd") -Destination (Join-Path $out "Uninstall.cmd")
Copy-Item -LiteralPath (Join-Path $root "packaging\sample.csv") -Destination (Join-Path $out "sample.csv")
Copy-Item -LiteralPath (Join-Path $root "packaging\sample.xlsx") -Destination (Join-Path $out "sample.xlsx")

# A small sample document so the package is usable without other files.
$cli = Join-Path $root "target\release\quire.exe"
if (Test-Path -LiteralPath $cli) {
    & $cli new (Join-Path $out "sample.docx") | Out-Null
    if ($LASTEXITCODE -ne 0) { throw "sample document generation failed" }
}

$zip = Join-Path $dist "Quire-$Version-windows-x64.zip"
if (Test-Path -LiteralPath $zip) {
    Remove-Item -LiteralPath $zip -Force
}
Compress-Archive -Path (Join-Path $out "*") -DestinationPath $zip -Force

Write-Host "package: $out"
Write-Host "zip:     $zip"

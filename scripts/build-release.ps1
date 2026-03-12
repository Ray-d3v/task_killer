$ErrorActionPreference = "Stop"

$root = Split-Path -Parent $PSScriptRoot
$dist = Join-Path $root "dist"

New-Item -ItemType Directory -Force -Path $dist | Out-Null

Push-Location $root
try {
    cargo build --release

    Copy-Item -Force "target\release\tasktui-app.exe" (Join-Path $dist "tasktui-app.exe")
    Copy-Item -Force "target\release\tasktuictl.exe" (Join-Path $dist "tasktuictl.exe")
    Copy-Item -Force "target\release\updater.exe" (Join-Path $dist "updater.exe")
    Copy-Item -Force "target\release\uninstall.exe" (Join-Path $dist "uninstall.exe")
    Copy-Item -Force "target\release\tasktui-service.exe" (Join-Path $dist "tasktui-service.exe")
    Copy-Item -Force "README.md" (Join-Path $dist "README.txt")
}
finally {
    Pop-Location
}

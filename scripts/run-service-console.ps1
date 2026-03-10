$ErrorActionPreference = "Stop"

$root = Split-Path -Parent $PSScriptRoot
$serviceExe = Join-Path $root "target\debug\tasktui-service.exe"

if (-not (Test-Path $serviceExe)) {
    $serviceExe = Join-Path $root "target\release\tasktui-service.exe"
}

if (-not (Test-Path $serviceExe)) {
    throw "Service executable not found. Build the workspace first."
}

& $serviceExe --console

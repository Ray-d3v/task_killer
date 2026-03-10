$ErrorActionPreference = "Stop"

$root = Split-Path -Parent $PSScriptRoot
$serviceExe = Join-Path $root "target\release\tasktui-service.exe"

if (-not (Test-Path $serviceExe)) {
    throw "Service executable not found: $serviceExe"
}

sc.exe create tasktui-service binPath= "`"$serviceExe`"" start= auto obj= LocalSystem
sc.exe description tasktui-service "tasktui privileged backend service"
sc.exe start tasktui-service

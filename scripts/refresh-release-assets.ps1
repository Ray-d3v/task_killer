$ErrorActionPreference = "Stop"

$root = Split-Path -Parent $PSScriptRoot
$cargoToml = Join-Path $root "Cargo.toml"
$dist = Join-Path $root "dist"
$portableDir = Join-Path $dist "portable"
$versionLine = Select-String -Path $cargoToml -Pattern '^version = "(.+)"$' | Select-Object -First 1
if (-not $versionLine) {
    throw "Could not read workspace version from Cargo.toml"
}

$version = $versionLine.Matches[0].Groups[1].Value
$zipPath = Join-Path $dist "task_killer-$version-x64-portable.zip"
$msiPath = Join-Path $dist "task_killer-$version-x64.msi"
$shaFile = Join-Path $dist "SHA256SUMS.txt"

if (Test-Path $portableDir) {
    Remove-Item -Recurse -Force $portableDir
}

New-Item -ItemType Directory -Force -Path $portableDir | Out-Null

Copy-Item -Force (Join-Path $dist "tasktui-app.exe") (Join-Path $portableDir "tasktui-app.exe")
Copy-Item -Force (Join-Path $dist "tasktuictl.exe") (Join-Path $portableDir "tasktuictl.exe")
Copy-Item -Force (Join-Path $dist "updater.exe") (Join-Path $portableDir "updater.exe")
Copy-Item -Force (Join-Path $dist "uninstall.exe") (Join-Path $portableDir "uninstall.exe")
Copy-Item -Force (Join-Path $dist "tasktui-service.exe") (Join-Path $portableDir "tasktui-service.exe")
Copy-Item -Force (Join-Path $dist "README.txt") (Join-Path $portableDir "README.txt")
Copy-Item -Force (Join-Path $root "scripts\install-service.ps1") (Join-Path $portableDir "install-service.ps1")
Copy-Item -Force (Join-Path $root "scripts\uninstall-service.ps1") (Join-Path $portableDir "uninstall-service.ps1")

if (Test-Path $zipPath) {
    Remove-Item -Force $zipPath
}

Compress-Archive -Path (Join-Path $portableDir "*") -DestinationPath $zipPath -CompressionLevel Optimal

$lines = foreach ($file in @($zipPath, $msiPath)) {
    $hash = (Get-FileHash -Path $file -Algorithm SHA256).Hash.ToLowerInvariant()
    "{0} *{1}" -f $hash, (Split-Path -Leaf $file)
}

$lines | Set-Content -Path $shaFile -Encoding ascii

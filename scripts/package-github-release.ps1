param(
    [switch]$SkipSigning
)

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
$zipName = "task_killer-$version-x64-portable.zip"
$msiName = "task_killer-$version-x64.msi"
$zipPath = Join-Path $dist $zipName
$msiPath = Join-Path $dist $msiName
$shaFile = Join-Path $dist "SHA256SUMS.txt"

if ($SkipSigning) {
    & (Join-Path $PSScriptRoot "build-installer.ps1") -SkipSigning
} else {
    & (Join-Path $PSScriptRoot "build-installer.ps1")
}

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

$hashTargets = @($zipPath, $msiPath)
$lines = foreach ($file in $hashTargets) {
    $hash = (Get-FileHash -Path $file -Algorithm SHA256).Hash.ToLowerInvariant()
    "{0} *{1}" -f $hash, (Split-Path -Leaf $file)
}
$lines | Set-Content -Path $shaFile -Encoding ascii

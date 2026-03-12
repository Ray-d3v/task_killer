$ErrorActionPreference = "Stop"

$root = Split-Path -Parent $PSScriptRoot
$cargoToml = Join-Path $root "Cargo.toml"
$dist = Join-Path $root "dist"
$wixPath = Join-Path $env:USERPROFILE ".dotnet\tools"
$env:PATH = "$env:PATH;$wixPath"

if (-not (Get-Command wix -ErrorAction SilentlyContinue)) {
    throw "WiX Toolset is not installed. Run 'dotnet tool install --global wix' first."
}

& wix extension add --global WixToolset.Util.wixext/6.0.2 | Out-Null

$versionLine = Select-String -Path $cargoToml -Pattern '^version = "(.+)"$' | Select-Object -First 1
if (-not $versionLine) {
    throw "Could not read workspace version from Cargo.toml"
}

$version = $versionLine.Matches[0].Groups[1].Value
$msiName = "task_killer-$version-x64.msi"
$msiPath = Join-Path $dist $msiName

& (Join-Path $PSScriptRoot "build-release.ps1")

New-Item -ItemType Directory -Force -Path $dist | Out-Null

Push-Location $root
try {
    wix build `
        -arch x64 `
        -ext WixToolset.Util.wixext `
        -d ProductVersion=$version `
        installer\task_killer.wxs `
        -o $msiPath

    & (Join-Path $PSScriptRoot "sign-artifacts.ps1")
}
finally {
    Pop-Location
}

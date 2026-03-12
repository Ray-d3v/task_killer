$ErrorActionPreference = "Stop"

$root = Split-Path -Parent $PSScriptRoot
$cargoToml = Join-Path $root "Cargo.toml"
$dist = Join-Path $root "dist"

function Get-WorkspaceVersion {
    $versionLine = Select-String -Path $cargoToml -Pattern '^version = "(.+)"$' | Select-Object -First 1
    if (-not $versionLine) {
        throw "Could not read workspace version from Cargo.toml"
    }
    return $versionLine.Matches[0].Groups[1].Value
}

function Find-SignTool {
    $command = Get-Command signtool.exe -ErrorAction SilentlyContinue
    if ($command) {
        return $command.Source
    }

    $kitsRoot = "C:\Program Files (x86)\Windows Kits\10\bin"
    if (-not (Test-Path $kitsRoot)) {
        throw "signtool.exe was not found. Install the Windows SDK or add signtool.exe to PATH."
    }

    $candidate = Get-ChildItem $kitsRoot -Recurse -Filter signtool.exe -ErrorAction SilentlyContinue |
        Where-Object { $_.FullName -match '\\x64\\signtool\.exe$' } |
        Sort-Object FullName -Descending |
        Select-Object -First 1

    if (-not $candidate) {
        throw "signtool.exe was not found. Install the Windows SDK or add signtool.exe to PATH."
    }

    return $candidate.FullName
}

function Get-RequiredEnv([string]$name) {
    $value = [Environment]::GetEnvironmentVariable($name)
    if ([string]::IsNullOrWhiteSpace($value)) {
        throw "Missing required environment variable: $name"
    }
    return $value
}

function Invoke-SignTool {
    param(
        [string]$SignToolPath,
        [string]$FilePath,
        [string]$CertPath,
        [string]$CertPassword,
        [string]$TimestampUrl,
        [string]$FileDigest,
        [string]$TimestampDigest
    )

    & $SignToolPath sign `
        /fd $FileDigest `
        /td $TimestampDigest `
        /tr $TimestampUrl `
        /f $CertPath `
        /p $CertPassword `
        /d "Task Killer" `
        /v `
        $FilePath

    if ($LASTEXITCODE -ne 0) {
        throw "signtool sign failed for $FilePath"
    }
}

function Invoke-VerifyTool {
    param(
        [string]$SignToolPath,
        [string]$FilePath
    )

    & $SignToolPath verify /pa /v $FilePath
    if ($LASTEXITCODE -ne 0) {
        throw "signtool verify failed for $FilePath"
    }
}

$version = Get-WorkspaceVersion
$signTool = Find-SignTool
$certPath = Get-RequiredEnv "TASK_KILLER_SIGN_CERT_PATH"
$certPassword = Get-RequiredEnv "TASK_KILLER_SIGN_CERT_PASSWORD"
$timestampUrl = Get-RequiredEnv "TASK_KILLER_SIGN_TIMESTAMP_URL"
$fileDigest = [Environment]::GetEnvironmentVariable("TASK_KILLER_SIGN_FILE_DIGEST")
$timestampDigest = [Environment]::GetEnvironmentVariable("TASK_KILLER_SIGN_TIMESTAMP_DIGEST")

if ([string]::IsNullOrWhiteSpace($fileDigest)) {
    $fileDigest = "sha256"
}
if ([string]::IsNullOrWhiteSpace($timestampDigest)) {
    $timestampDigest = "sha256"
}

if (-not (Test-Path $certPath)) {
    throw "Signing certificate file not found: $certPath"
}

$targets = @(
    (Join-Path $dist "tasktui-app.exe"),
    (Join-Path $dist "tasktuictl.exe"),
    (Join-Path $dist "tasktui-service.exe"),
    (Join-Path $dist "task_killer-$version-x64.msi")
)

foreach ($target in $targets) {
    if (-not (Test-Path $target)) {
        throw "Signing target not found: $target"
    }
}

foreach ($target in $targets) {
    Invoke-SignTool `
        -SignToolPath $signTool `
        -FilePath $target `
        -CertPath $certPath `
        -CertPassword $certPassword `
        -TimestampUrl $timestampUrl `
        -FileDigest $fileDigest `
        -TimestampDigest $timestampDigest

    Invoke-VerifyTool -SignToolPath $signTool -FilePath $target
}

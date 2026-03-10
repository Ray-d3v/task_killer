$ErrorActionPreference = "Stop"

$root = Split-Path -Parent $PSScriptRoot

Push-Location $root
try {
    cargo run -p tasktui-app --bin tasktuictl -- ping
} finally {
    Pop-Location
}

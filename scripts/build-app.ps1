# Builds the Colony Command desktop app and its daemon (release).
# Output: colony-command.exe, colonyd.exe, and colony-ptyd.exe in target/release.
$ErrorActionPreference = "Stop"
$root = Split-Path $PSScriptRoot -Parent
Push-Location (Join-Path $root "app")
try { npm run build } finally { Pop-Location }
Push-Location $root
try { cargo build --release -p colony-app -p colonyd -p colony-ptyd --features colony-app/custom-protocol } finally { Pop-Location }
Write-Host "Built $root\target\release\colony-command.exe"

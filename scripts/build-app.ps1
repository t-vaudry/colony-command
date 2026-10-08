# Builds the Colony Command desktop app and its daemon (release).
# Output: target\release\colony-command.exe and colonyd.exe, side by side.
$ErrorActionPreference = "Stop"
$root = Split-Path $PSScriptRoot -Parent
Push-Location (Join-Path $root "app")
try { npm run build } finally { Pop-Location }
Push-Location $root
try { cargo build --release -p colony-app -p colonyd --features colony-app/custom-protocol } finally { Pop-Location }
Write-Host "Built $root\target\release\colony-command.exe"

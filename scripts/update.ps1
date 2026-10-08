# Rebuilds Colony and restarts it without ending your sessions.
#
# Stops the map window and colonyd, rebuilds them, and relaunches the app.
# Sessions live in colony-ptyd, which keeps running, so they survive. Pass
# -IncludeTerminalHost to rebuild colony-ptyd too; that does end its sessions.
# If colony-ptyd isn't running (first update after it was introduced), it is
# built as well.
param([switch]$IncludeTerminalHost)
$ErrorActionPreference = "Stop"
$root = Split-Path $PSScriptRoot -Parent
$env:PATH = "C:\Program Files\nodejs;$env:USERPROFILE\.cargo\bin;$env:PATH"

Get-Process colony-command, colonyd -ErrorAction SilentlyContinue | Stop-Process -Force
$ptyd = Get-Process colony-ptyd -ErrorAction SilentlyContinue
if ($IncludeTerminalHost -and $ptyd) { $ptyd | Stop-Process -Force; $ptyd = $null }
Start-Sleep -Milliseconds 500

Push-Location (Join-Path $root "app")
try { npm run build } finally { Pop-Location }
$crates = @("-p", "colony-app", "-p", "colonyd")
if (-not $ptyd) { $crates += @("-p", "colony-ptyd") }
Push-Location $root
try { cargo build --release @crates --features colony-app/custom-protocol } finally { Pop-Location }

Start-Process (Join-Path $root "target\release\colony-command.exe")
Write-Host "Colony updated and relaunched."

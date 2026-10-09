# Rebuilds Colony and restarts it without ending your sessions.
#
# Stops the map window and colonyd, rebuilds them, and relaunches the app.
# Sessions live in colony-ptyd, which keeps running, so they survive. Pass
# -IncludeTerminalHost to rebuild colony-ptyd too; that does end its sessions.
# It refuses while sessions are live in it, unless -Force is also given.
# If colony-ptyd isn't running (first update after it was introduced), it is
# built as well.
#
# The app is relaunched through Explorer, so it doesn't belong to this
# console: closing this window won't take Colony down with it, and an
# administrator prompt won't make Colony (or the sessions it starts) run as
# administrator.
param([switch]$IncludeTerminalHost, [switch]$Force)
$ErrorActionPreference = "Stop"
$root = Split-Path $PSScriptRoot -Parent
$env:PATH = "C:\Program Files\nodejs;$env:USERPROFILE\.cargo\bin;$env:PATH"

function Stop-AndWait($procs) {
    foreach ($p in $procs) {
        try { Stop-Process -Id $p.Id -Force -ErrorAction Stop } catch { Write-Warning "couldn't stop $($p.ProcessName) ($($p.Id)): $($_.Exception.Message)" }
    }
    foreach ($p in $procs) {
        try { Wait-Process -Id $p.Id -Timeout 10 -ErrorAction Stop } catch [System.TimeoutException] {
            throw "$($p.ProcessName) ($($p.Id)) is still running; close it and run this again."
        } catch {}
    }
}

# Sessions in colony-ptyd die with it: don't go on without being told to.
if ($IncludeTerminalHost -and (Get-Process colony-ptyd -ErrorAction SilentlyContinue) -and -not $Force) {
    $live = $null
    try {
        $home_ = if ($env:COLONY_HOME) { $env:COLONY_HOME } else { Join-Path $env:USERPROFILE ".colony" }
        $info = Get-Content (Join-Path $home_ "daemon.json") -Raw | ConvertFrom-Json
        $agents = Invoke-RestMethod "http://127.0.0.1:$($info.port)/api/agents?token=$($info.token)" -TimeoutSec 5
        $live = @($agents.agents | Where-Object { $_.terminal }).Count
    } catch {}
    if ($null -eq $live -or $live -gt 0) {
        $what = if ($null -eq $live) { "can't tell whether sessions are live in colony-ptyd" } else { "$live session(s) are live in colony-ptyd" }
        throw "$what, and restarting it ends them. Run again with -Force to do it anyway."
    }
}

Stop-AndWait @(Get-Process colony-command, colonyd -ErrorAction SilentlyContinue)
$ptyd = Get-Process colony-ptyd -ErrorAction SilentlyContinue
if ($IncludeTerminalHost -and $ptyd) { Stop-AndWait @($ptyd); $ptyd = $null }

Push-Location (Join-Path $root "app")
try {
    npm run build
    if ($LASTEXITCODE -ne 0) { throw "the map failed to build (npm exit $LASTEXITCODE)" }
} finally { Pop-Location }

$crates = @("-p", "colony-app", "-p", "colonyd")
if (-not $ptyd) { $crates += @("-p", "colony-ptyd") }
Push-Location $root
try {
    cargo build --release @crates --features colony-app/custom-protocol
    if ($LASTEXITCODE -ne 0) { throw "the build failed (cargo exit $LASTEXITCODE); Colony was not relaunched" }
} finally { Pop-Location }

# Through Explorer: outside this console's process group, at normal rights.
Start-Process explorer.exe -ArgumentList "`"$(Join-Path $root 'target\release\colony-command.exe')`""
Write-Host "Colony updated and relaunched."

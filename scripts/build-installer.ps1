# Builds the Colony Command installer (NSIS, per-user): target/release/bundle/nsis/*.exe
#
# It contains the app, colonyd, colony-ptyd, colony-hook and colony-setup, plus the
# Linux colony-probe and colony-hook (static musl) that "Set up Colony" copies into
# WSL distros. The Linux binaries come from, in order:
#   -LinuxDir <dir>   a folder holding colony-probe and colony-hook (CI artifact)
#   a WSL distro      built there with the musl target (needs rustup in that distro)
# or pass -SkipLinux to build an installer that can't set up WSL.
param(
    [string]$LinuxDir,
    [string]$Distro = "Ubuntu",
    [switch]$SkipLinux
)
$ErrorActionPreference = "Stop"
$root = Split-Path $PSScriptRoot -Parent
$tauri = Join-Path $root "app\src-tauri"
$env:PATH = "C:\Program Files\nodejs;$env:USERPROFILE\.cargo\bin;$env:PATH"
. (Join-Path $PSScriptRoot "build-prereqs.ps1")

# Windows binaries, named the way Tauri's externalBin wants them.
$triple = (rustc -vV | Select-String '^host: (.+)$').Matches[0].Groups[1].Value
Push-Location $root
try {
    cargo build --release -p colonyd -p colony-ptyd -p colony-hook -p colony-setup
    if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }
} finally { Pop-Location }
New-Item -ItemType Directory -Force (Join-Path $tauri "binaries") | Out-Null
foreach ($b in "colonyd", "colony-ptyd", "colony-hook", "colony-setup") {
    Copy-Item (Join-Path $root "target\release\$b.exe") (Join-Path $tauri "binaries\$b-$triple.exe") -Force
}

# Linux binaries for WSL.
$dest = Join-Path $tauri "resources\linux\x86_64"
if (-not $SkipLinux) {
    New-Item -ItemType Directory -Force $dest | Out-Null
    if ($LinuxDir) {
        foreach ($b in "colony-probe", "colony-hook") { Copy-Item (Join-Path $LinuxDir $b) $dest -Force }
    } else {
        $wslRoot = (wsl -d $Distro --exec wslpath -u $root).Trim()
        $script = 'set -e; . "$HOME/.cargo/env"; rustup target add x86_64-unknown-linux-musl; export CARGO_TARGET_DIR="$HOME/.cache/colony-musl"; cd "$1"; cargo build --release --target x86_64-unknown-linux-musl -p colony-probe -p colony-hook; for b in colony-probe colony-hook; do cp "$CARGO_TARGET_DIR/x86_64-unknown-linux-musl/release/$b" "$2/"; done'
        $wslDest = (wsl -d $Distro --exec wslpath -u $dest).Trim()
        wsl -d $Distro --exec sh -c $script sh $wslRoot $wslDest
        if ($LASTEXITCODE -ne 0) { throw "building the Linux binaries in $Distro failed; pass -LinuxDir or -SkipLinux" }
    }
} else {
    Write-Warning "-SkipLinux: this installer can't set up WSL distros."
}

Push-Location (Join-Path $root "app")
try {
    $configs = @("--config", "src-tauri/tauri.bundle.conf.json")
    # Updater artifacts (.sig) need the signing key; local builds without it skip them.
    if ($env:TAURI_SIGNING_PRIVATE_KEY) { $configs += @("--config", "src-tauri/tauri.updater.conf.json") }
    npm run tauri -- build --bundles nsis @configs
    if ($LASTEXITCODE -ne 0) { throw "tauri build failed" }
} finally { Pop-Location }
Get-ChildItem (Join-Path $root "target\release\bundle\nsis\*.exe") | ForEach-Object { Write-Host "Built $($_.FullName)" }

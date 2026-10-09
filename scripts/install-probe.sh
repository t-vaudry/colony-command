#!/bin/bash
# Builds colony-probe inside the current WSL distro and installs it to
# ~/.colony/bin, where colonyd looks for it. Run inside each distro:
#   wsl -d Ubuntu -- bash scripts/install-probe.sh
set -euo pipefail
source "$HOME/.cargo/env"
export CARGO_TARGET_DIR="$HOME/.cache/colony-target"
cd "$(dirname "$0")/.."
cargo build --release -p colony-probe
mkdir -p "$HOME/.colony/bin"
install -m 755 "$CARGO_TARGET_DIR/release/colony-probe" "$HOME/.colony/bin/colony-probe"
echo "installed $HOME/.colony/bin/colony-probe"
install -m 755 hooks/colony-approve.sh "$HOME/.colony/bin/colony-approve.sh"
echo "installed $HOME/.colony/bin/colony-approve.sh"
echo
echo "To get approvals on the map from this distro, register the hook in ~/.claude/settings.json"
echo "under hooks.PermissionRequest (restart the probe by restarting colonyd):"
echo '  { "type": "command", "command": "sh \"$HOME/.colony/bin/colony-approve.sh\"", "timeout": 600 }'

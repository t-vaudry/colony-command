#!/bin/bash
source "$HOME/.cargo/env"
export CARGO_TARGET_DIR="$HOME/.cache/colony-target"
cd /mnt/c/Users/thoma/code/colony-command
cargo test --workspace "$@"

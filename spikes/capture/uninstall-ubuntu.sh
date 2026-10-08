#!/bin/bash
# Restores the Ubuntu settings.json saved before the capture hook was installed.
S="$HOME/.claude/settings.json"
if [ -f "$S.colony-backup" ]; then mv "$S.colony-backup" "$S" && echo "ubuntu: restored backup"; else echo "ubuntu: no backup found"; fi

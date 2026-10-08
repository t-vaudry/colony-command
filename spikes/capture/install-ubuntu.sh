#!/bin/bash
set -e
S="$HOME/.claude/settings.json"
[ -f "$S.colony-backup" ] || cp "$S" "$S.colony-backup"
cp "$(dirname "$0")/settings.ubuntu.json" "$S"
python3 -c "import json,sys; d=json.load(open('$S')); print('ubuntu: installed, theme =', d.get('theme'), ', hook events =', len(d['hooks']))"

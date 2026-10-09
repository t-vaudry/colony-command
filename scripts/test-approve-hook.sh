#!/bin/sh
# Checks hooks/colony-approve.sh in its failure modes: it must print nothing,
# exit 0, and return quickly. Run: sh scripts/test-approve-hook.sh
hook="$(cd "$(dirname "$0")/.." && pwd)/hooks/colony-approve.sh"
fail=0
check() { # name, home
  start=$(date +%s)
  out=$(echo '{"hook_event_name":"PermissionRequest"}' | HOME="$2" sh "$hook"); rc=$?
  took=$(( $(date +%s) - start ))
  if [ "$rc" = 0 ] && [ -z "$out" ] && [ "$took" -lt 5 ]; then echo "ok   $1"; else echo "FAIL $1 (rc=$rc out='$out' ${took}s)"; fail=1; fi
}
t=$(mktemp -d)
check "no colonyd at all" "$t"
mkdir -p "$t/.colony"
echo '{"port": 1, "token": "abc"}' > "$t/.colony/daemon.json"
check "daemon.json points at a dead port" "$t"
echo 'garbage' > "$t/.colony/daemon.json"
check "unreadable daemon.json" "$t"
rm "$t/.colony/daemon.json"
# A stale socket file (probe died without cleaning up).
python3 -c "import socket,sys; s=socket.socket(socket.AF_UNIX); s.bind(sys.argv[1])" "$t/.colony/hook.sock" 2>/dev/null \
  && check "stale hook socket" "$t"
rm -rf "$t"
exit $fail

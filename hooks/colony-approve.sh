#!/bin/sh
# Colony Command approval hook, registered for Claude Code's PermissionRequest
# event. Hands the request (JSON on stdin) to colonyd, which holds it until
# someone answers on the map, then prints colonyd's decision for Claude Code.
#
# Fails open: if colonyd isn't running, nobody has the map open, or no answer
# comes, it prints nothing and Claude Code's own permission prompt decides.
info="$HOME/.colony/daemon.json"
[ -f "$info" ] || exit 0
port=$(sed -n 's/.*"port": *\([0-9][0-9]*\).*/\1/p' "$info")
token=$(sed -n 's/.*"token": *"\([0-9a-f]*\)".*/\1/p' "$info")
[ -n "$port" ] && [ -n "$token" ] || exit 0
curl -s --max-time 590 -H "content-type: application/json" --data-binary @- \
  "http://127.0.0.1:$port/api/permission?token=$token" 2>/dev/null
exit 0

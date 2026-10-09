# Capture spike

Records every Claude Code hook payload to `~/.colony/capture/<nanotime>-<pid>.json`
on Windows and in Ubuntu, so the real event schema can be checked before
`colony-hook` is written. The hook returns no decision; sessions behave as before.

- `hooks-capture.json` — the Windows `~/.claude/settings.json` as installed
- `settings.ubuntu.json` — the Ubuntu settings as installed (adds `theme`)
- Uninstall: `powershell -File uninstall-windows.ps1` and, in Ubuntu, `bash uninstall-ubuntu.sh`

Superseded by `crates/colony-hook`.

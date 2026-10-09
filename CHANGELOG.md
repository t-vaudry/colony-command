# Changelog

The release workflow publishes the section matching the tag as the GitHub Release
notes, and `scripts/check-release-version.sh vX.Y.Z` refuses a tag with no section here.

## [1.0.0] - first tagged release

The first release meant for people other than its author. It rolls up the V2
roadmap releases 1.1 to 1.3 (`docs/design-spec.html`, section 13) and the installer.
The installer is **not code-signed**: Windows SmartScreen warns about an unknown
publisher until the file has built up reputation (see the README, Install).

### Install and update

- Per-user NSIS installer (no administrator prompt) bundling the app, `colonyd`,
  `colony-ptyd`, `colony-hook`, `colony-setup` and the static Linux probe and hook.
- **Set up Colony** window and `colony-setup` command line: registers the hooks in
  `~/.claude/settings.json` on Windows and in each WSL distro, copies the probe and
  approval hook into distros, with a diff review and a timestamped backup first.
  Your own settings are never reformatted; invalid files are refused. It names any
  missing binary and fails instead of doing nothing, and re-offers itself when the
  hooks or probe are older than the app.
- Uninstall offers to remove Colony's hooks with the same backup-first edit.
- Automatic updates (Tauri updater): signed releases on GitHub, an Install / Later
  notice, and an upgrade that leaves running `colony-ptyd` sessions alive.
- Development: `tauri dev` stages the sidecars; `scripts/update.ps1` rebuilds and
  relaunches without ending sessions.

### 1.1 Act from the map

- Answer permission requests on the map (Allow / Deny), for Windows and WSL sessions.
- "Allow always for project" rules and a saved-rules panel (`policy.json`).
- Pause and resume a session; reply to a waiting session from the map; one-click
  resume for adopted questions.
- Interrupted sessions are kept and resumed after a restart.
- Attention budget: Focus mode, a motion ceiling and a reduced-motion override.
- Patience budgets and OS notifications for bots that have waited too long.
- Tray icon, close-to-tray, start at login and an approvals kill switch.

### 1.2 Know where agents work

- Buildings per working directory on the map.
- Edit-collision warnings when two agents touch the same files; git diff stat on
  work that is ready to review.
- Side panel: resizable, scrollable text, activity feed, one action row, Markdown
  in questions and permissions.
- Per-project token and cost tracking (top bar, district headers, inspector).
- Control for sessions Colony adopted: Kill, Focus terminal, origin marker.
- Offers to sign Claude in or install a missing program when a bot is blocked.

### 1.3 Look back

- Time-lapse replay from a daemon event log.
- End-of-day summary in the side panel.
- Per-project cost charts and human-latency charts (median time to respond).
- Local speech-to-text dictation in the message boxes.

### Known limits

- Windows 10/11 x64 only; no code signing yet; no aarch64 Linux probe for WSL on ARM.
- Hooks are POSIX shell commands, so Windows needs Claude Code's default shell
  (Git Bash).

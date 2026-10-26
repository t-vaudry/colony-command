# Changelog

The release workflow publishes the section matching the tag as the GitHub Release
notes, and `scripts/check-release-version.sh vX.Y.Z` refuses a tag with no section here.

## [Unreleased]

## [0.1.7] - 2026-10-26

### Fixed

- Updating Colony no longer leaves an older background service (colonyd) running: the app now replaces a colonyd from a different version (your sessions keep running) and says so, instead of showing "unknown variant" errors and no crews. If it can't be stopped safely, the map shows an "out of date" banner with a Restart button.

## [0.1.6] - 2026-10-26

- Crews: each bot's panel shows whether its crew is used (searches delegated or done by hand, swarm agents that ran out of turns, advisor use, and whether nudges were followed), with a one-line roll-up in the day summary.
- Context watch: each bot's panel shows how full its context is ("Context 142k / 200k · 71%"). Past 70% of the window (configurable) Colony says so once, offers a Compact button for bots it started, and nudges crew sessions to hand searches and test runs to the swarm.
- Budgets: set an optional spend cap per bot and per day in a bot's Session details. You get a notice at 80%, and at 100% a bot Colony started drops its advisor (then moves down a model tier) after its current turn, with Raise cap and Undo.
- Crews: the New session dialog suggests a crew from your first message (a new lighter "Haiku builds · Sonnet reviews" crew for small fixes and questions, checked locally with no API calls), and a folder can have its own default crew, kept in Colony's settings rather than the repository.
- Crews: bots can use a swarm of cheap Haiku helpers for searching and auditing, with Opus advising only at key decisions, to spend far fewer tokens.
- Crews: the New session dialog picks a crew instead of a model. The default
  crew is Sonnet builds, Haiku swarms, Opus reviews (Opus attached as the
  advisor); you can customize the models for one bot, save your own crews, and
  a pairing Claude Code would reject is refused before Start.
- Crews: in sessions with an advisor, the hook nudges Claude to consult Opus when the
  same error happens twice, before a plan locks, and once before it finishes a turn
  that changed files. These are nudges: the first ExitPlanMode of a turn is bounced once, nothing is blocked for good.
- Cost by model: a bot's usage and the day's total now show where the money
  went (for example Sonnet, Haiku and an Opus advisor) with the Opus share,
  advisor tokens are priced at the advisor's rates, each advisor consult shows
  on the map, and Colony no longer suggests Opus for planning when an Opus
  advisor is attached.

## [0.1.5] - 2026-10-21

- Sessions from before: at startup Colony finds sessions in Colony worktrees,
  in any repository, on Windows and running WSL distros, that it had lost track
  of, and lists them. The ones you pick come back as paused bots with Resume
  ready.

## [0.1.4] - 2026-10-20

- Bots come back after a reboot: every session on the map that hadn't ended
  returns paused, in its project and worktree, with Resume ready, until it is
  resumed or dismissed. Sessions Colony started before this release, and ones
  with a worktree, come back too.

## [0.1.3] - 2026-10-19

- Main moving in one repo only reaches bots of that repo (#46)
- Side-panel menus close on an outside click or Escape (#45)

## [0.1.2] - 2026-10-14

### Fixed

- WSL bots approved on the map stayed on the front porch while they worked. Inside
  a distro `colony-hook` found no `daemon.json` and spooled every event, which the
  probe only read when it restarted; it now writes live captures while the probe is
  listening.
- A permission allowed or denied on the map sends the bot back to its district at
  once, instead of when the approved tool finishes (a long build looked stuck).

## [0.1.1] - first update-tested release

The first release meant for people other than its author, and the first to exercise the in-app updater (0.1.0 → 0.1.1). It rolls up the V2
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

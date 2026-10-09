# Colony Command

A desktop app that shows every Claude Code session (Windows terminals, WSL,
and the Claude desktop app) as a bot on a colony map, so you can see at a
glance which agents are working, blocked, waiting on you, or done.

Design spec: [`docs/design-spec.html`](docs/design-spec.html)

## Install

For using Colony, not developing it. Windows 10/11, x64. Claude Code must already
be installed (on Windows, in WSL, or both).

1. Download `Colony Command_<version>_x64-setup.exe` (from the repository's
   Releases page, or the `colony-installer` artifact of a tagged CI run).
   Windows SmartScreen will say the publisher is unknown: the installer isn't
   code-signed yet. *More info → Run anyway*.
2. Run it. It installs for your user only (no administrator prompt) under
   `%LOCALAPPDATA%\Colony Command` and adds a Start menu entry.
3. Start **Colony Command**. On first launch the **Set up Colony** window opens
   (it is also the **Set up Colony** button at the map's bottom-left corner).
   It lists Windows and each WSL distro (running or stopped) with a checklist:

   | Item | What it does |
   |---|---|
   | Hooks | Registers `colony-hook` for every Claude Code hook event in `~/.claude/settings.json`, so sessions appear on the map |
   | Probe (WSL) | Copies `colony-probe` to `~/.colony/bin` in the distro; it streams that distro's sessions to Colony. No Rust toolchain needed |
   | Approvals | Lets permission requests be answered on the map (Allow / Deny). On Windows this is `colony-hook` with the long timeout; in WSL it also copies `colony-approve.sh` and registers it for `PermissionRequest` |

   **Review changes** shows the exact edit to each `settings.json` as a diff and the
   files that will be copied. Nothing is written until you press **Apply**. Before
   any write, the old file is copied to `settings.json.colony-backup-<timestamp>`.
   Your own hooks and settings are never reformatted or reordered, and an invalid
   `settings.json` is refused rather than overwritten. A stopped distro is only
   started if you tick it.
4. The window re-opens after an update when installed hooks or the probe are older
   than the app. **Not now** hides it until the next version; the button stays.

Windows hooks are POSIX shell commands, so they need Claude Code's default shell on Windows (Git Bash, which Claude Code requires). Colony's entries are tagged with a `# colony-setup v=<version>` comment in their
command, which is how it finds, repairs, upgrades and removes them. They are
written so a broken or missing Colony can't get in Claude Code's way: if the
binary isn't there the command does nothing and exits 0, and Colony's binaries
never exit with the code that blocks a tool call.

### Updating

For now updating is manual: download the new installer and run it over the old
one. It keeps your settings, replaces the app files (a running `colonyd` is
stopped and restarts with the app; sessions in `colony-ptyd` keep running), and
Set up Colony re-offers to refresh the hooks and probe. See the TODO under
*Release and CI* for automatic updates.

### Uninstalling

*Settings → Apps → Colony Command → Uninstall*. It asks whether to also remove
Colony's hooks from Claude Code's settings (Windows and running WSL distros);
answer Yes to do it with the same backup-first edit. To remove them later, or if
the app is already gone, run `%USERPROFILE%\.colony\bin\colony-setup.exe uninstall`,
or untick items in Set up Colony. By hand: delete the entries in
`~/.claude/settings.json` whose command ends in `# colony-setup ...` (leftover
ones are harmless, they only run if `~/.colony/bin/colony-hook` still exists).
`~/.colony` (your history and logs) is left in place.

If you answer No to removing the hooks, the uninstaller still deletes `~/.colony/bin/colony-hook.exe` (including a copy you built yourself) so the leftover entries do nothing. A `colony-ptyd` that has sessions in it keeps running, renamed aside as `colony-ptyd.<n>.old`, so the install folder may remain until those sessions end.

### Command line

The same installer logic is available without the app:

    colony-setup status [--target windows|wsl:Ubuntu|all] [--json]
    colony-setup install   [--no-hooks] [--no-probe] [--no-approval] [--dry-run] [--yes]
    colony-setup uninstall [--dry-run] [--yes]

`--include-stopped` also reads stopped distros (starting them). For tests,
`--user-home`, `--colony-home`, `--wsl-home` and `--bundle` redirect it to a
scratch folder (also `COLONY_SETUP_USER_HOME`, `COLONY_HOME`,
`COLONY_SETUP_WSL_HOME`, `COLONY_BUNDLE_DIR`).

## Develop

Everything below is for working on Colony itself.

### Layout

| Path | What |
|---|---|
| `crates/colony-core` | Domain types and the state reducer (no I/O) |
| `crates/colony-source` | Watches the session registry and hook captures, emits events |
| `crates/colony-probe` | Runs in each WSL distro, streams its events to colonyd over stdio |
| `crates/colonyd` | Daemon: reducer + local WebSocket API on 127.0.0.1:7878 |
| `crates/colony-ptyd` | Terminal host: owns Colony's terminals so sessions survive daemon restarts |
| `app` | The map: PixiJS world + HUD, connects to colonyd |
| `app/src-tauri` | Desktop app: the map in a window; starts colonyd if it isn't running |
| `tools/synth`, `tools/replay` | Synthetic fleet generator and event-log replay, for load and visual tests |
| `hooks/colony-approve.sh` | Approval hook (Windows and WSL): hands permission requests to colonyd for Allow/Deny on the map |
| `crates/colony-hook` | Claude Code hook: records each hook payload for colonyd (spooling while it is away), keeps fixtures per Claude Code version |
| `crates/colony-setup` | Installer logic: edits `~/.claude/settings.json` (Windows and each WSL distro), copies the hook, probe and approval hook; `colony-setup` CLI. Used by the app's Set up Colony window and the uninstaller |
| `spikes/capture` | Superseded by `colony-hook`: shell hook that records raw payloads |
| `scripts/acceptance.mjs` | MVP acceptance measurements |
| `docs/` | Design spec, acceptance results |

## Test

From Windows (once Visual Studio C++ build tools are installed):

    cargo test --workspace

Or inside Ubuntu:

    wsl -d Ubuntu -- bash scripts/test-wsl.sh

## Run the app

    powershell -File scripts/build-app.ps1
    target/release/colony-command.exe

The app starts `colonyd` (next to it) when no daemon is answering, and
leaves it running when you close the window. Sessions Colony starts live in
`colony-ptyd`, which colonyd starts and reconnects to, so they keep running
through daemon restarts and updates. Logs: `~/.colony/colonyd.log`,
`~/.colony/ptyd.log`.

To pick up code changes without ending sessions:

    powershell -File scripts/update.ps1

It stops the window and colonyd, rebuilds, and relaunches; colony-ptyd and the
sessions in it keep running. `-IncludeTerminalHost` also rebuilds colony-ptyd,
which ends its sessions, so it refuses while any are live unless you add `-Force`.

Anything that stops colony-ptyd (`taskkill /IM colony-ptyd.exe`, an installer,
a reboot) ends the sessions in it: never stop it by image name, which also hits
the real one when testing. Sessions cut off that way are kept on the map as
crashed ("interrupted"), their worktrees are left alone, and colonyd resumes
the ones Colony started (`--resume`, same folder) up to 3 times, 30 s, 2 min
and 8 min apart (`~/.colony/sessions.json`). While terminals are live,
colony-ptyd also keeps Windows from sleeping on idle.

Set `COLONY_HOME` (and `COLONY_PORT` / `COLONY_PTYD_PORT`) to run an isolated
second Colony, e.g. for tests.

## Load and visual testing: `tools/synth` and `tools/replay`

Both feed a **test** colonyd through `POST /api/ingest`, which the daemon only
serves when started with `COLONY_INGEST=1` (otherwise 404). A real daemon never
takes made-up events.

    cargo build -p colonyd -p colony-synth -p colony-replay
    target/debug/colony-synth --spawn --agents 50 --speed 5 --record fleet.jsonl
    target/debug/colony-replay fleet.jsonl --spawn --speed 10

`--spawn` starts an isolated colonyd in a temp `COLONY_HOME` on a free port (its
user home is redirected too, and WSL distros aren't attached, so none of your
real sessions appear) and stops it on exit. To watch it on the map, run the app
with `COLONY_HOME` set to the folder `--spawn` prints. To feed a daemon you
started yourself, run it with `COLONY_INGEST=1` and its own `COLONY_HOME` /
`COLONY_PORT`, then pass `--home <that folder>`.

- `colony-synth`: `--agents`, `--projects`, `--speed`, `--duration`, `--seed`
  (same seed, same fleet). `--record` saves what it sent as JSON lines.
- `colony-replay <log.jsonl>`: `--speed` (0 = as fast as possible),
  `--keep-ts`, `--loop`. The log is one event envelope per line, oldest first.

## Acceptance checks

`node scripts/acceptance.mjs all` measures the MVP acceptance criteria (latency to the
map, fail-open hooks, daemon CPU/RAM, 50-session frame rate) against a throwaway
colonyd; it never touches your real one. Results and gaps: [`docs/acceptance.md`](docs/acceptance.md).

## Dismissing sessions

**Dismiss** in a bot's inspector ends every copy of that session (Colony's
terminal and any outside one) and takes it off the map right away. With no bot
selected, **Dismiss N idle bots** does the same for every idle, ended, or
crashed session; ones waiting for review are left alone. Dismissals are kept in
`~/.colony/dismissed.json` for a day, so a daemon restart doesn't bring them
back. The conversation stays on disk, and resuming it brings the bot back.

## Run the daemon by hand

    wsl -d Ubuntu -- bash scripts/install-probe.sh   # once per distro
    cargo run -p colonyd

It writes its port and access token to `~/.colony/daemon.json`. Running WSL
distros are attached automatically; stopped ones are left alone.

## Run the map

    cd app && npm install && npm run dev

Then open http://localhost:5173. In dev, the page reads the daemon's port and
token through a Vite route. Keys: `Space` next agent that needs you, `0` fit
the whole colony, `Esc` clear selection.

## Approvals from the map

Set up Colony does this for you (see Install). By hand, for development: register `hooks/colony-approve.sh` for Claude Code's `PermissionRequest` event
(copy it to `~/.colony/bin/`, then add to `~/.claude/settings.json`):

    { "type": "command", "command": "sh \"$HOME/.colony/bin/colony-approve.sh\"", "timeout": 600 }

While the map is open, permission requests show on the porch and in the
inspector with Allow, Always allow (adds Claude Code's suggested rule), Allow
always for project (see below), and Deny. The session's own prompt stays up
too; whichever is answered first wins. With colonyd stopped or no map open, the
hook steps aside.

### Allow always for project

Shown when Claude Code suggested an allow rule for the request (for example
`Bash(npm test)`). Clicking it allows this request and saves a rule in
`~/.colony/policy.json`, scoped to that project. Unlike Always allow, it does
not change Claude Code's own settings. A later request is allowed without
asking only when all of these hold:

- it comes from a session in the same project,
- it is the same tool, and Claude Code suggests exactly the same rule content
  for it (Colony never interprets or widens a pattern: `npm test` does not
  cover `npm test:*` or `npm run build`),
- it is not a question (`AskUserQuestion`) or plan approval.

Rules are only ever created by that click; Allow, Always allow, Deny, timeouts
and everything else write nothing. Rules apply even when no map is open (they
were your explicit choice); with no matching rule and no map the hook still
steps aside. A missing or unreadable `policy.json` means no rules.

The **saved rules** chip in the top bar, or the inspector with no bot selected,
lists every rule with the project it applies to and a Remove button. A removed
rule asks again.

This works for Windows and WSL sessions. In WSL, `scripts/install-probe.sh`
installs the hook next to the probe (`~/.colony/bin/colony-approve.sh`); add
the same settings entry in that distro's `~/.claude/settings.json`. The hook
talks to `colony-probe` over a private Unix socket (`~/.colony/hook.sock`,
owner-only), and the probe relays the request to colonyd over the channel
colonyd already has to it, so colonyd's token never enters the distro. Restart
colonyd (or the distro's probe) after re-running the installer. To check the
hook's fail-open behaviour: `sh scripts/test-approve-hook.sh`.

## Pause and resume

**Pause** in the inspector of a bot Colony started stops the session at a safe
point between turns: if it is working, it waits for the turn to end (shown as
"pausing after this turn", with Cancel pause); if it is already between turns,
it stops at once. Colony then ends the session's process; the conversation is
on disk, so **Resume** starts it again with `claude --resume` in a new Colony
terminal, on the same model and permission mode. Nothing is paused mid-tool,
while a permission request is open, or while a subagent or background run is
out.

A paused bot stays on the map, idle, with a pause mark (and "paused" in the
inspector) instead of ending or counting as crashed. Paused sessions are kept in
`~/.colony/paused.json`, so they survive a colonyd restart; they are forgotten
after 14 days or when dismissed.

Colony only pauses sessions it owns. For a session started in a terminal or the
Claude desktop app the inspector says why: stopping that process isn't Colony's
to do. Resume it in Colony to get Pause.

## Replying from the map

A bot with a question shows it in full in the inspector. For sessions Colony
started, the reply box pastes your answer into the session's terminal and
presses Enter; it finds the terminal by bot, so it keeps working after a colonyd
restart (the terminals live in `colony-ptyd`).

For a session Colony didn't start Colony can't type into it, so the question
card has a reply field and **Resume in Colony and send**: one click resumes the
conversation in a Colony terminal (no dialog) with your answer as its next
message, or **Resume in Colony** to resume without one. If the session is still
running in another terminal or the Claude desktop app, Colony first asks whether
to end that copy, so two copies never write to one conversation.

## Hook capture

`colony-hook` replaces the capture spike. Set up Colony registers it (see Install); to do it by hand, build it
(`cargo build --release -p colony-hook`), copy `target/release/colony-hook` to
`~/.colony/bin/`, and register it like the spike did: one
`{ "type": "command", "command": "<path>/colony-hook", "timeout": 5 }` entry for
each of SessionStart, SessionEnd, UserPromptSubmit, PreToolUse, PermissionRequest,
PostToolUse, PostToolUseFailure, Notification, SubagentStart, SubagentStop, Stop,
StopFailure, PreCompact, and PostCompact. For PermissionRequest use the longer
timeout from the approvals section; there it also stands in for
`colony-approve.sh` on Windows.

Each payload is written to `~/.colony/capture/` for colonyd. If colonyd isn't
running it goes to `~/.colony/spool/` instead, and the next colonyd start moves it
into `capture/`. The hook never blocks or breaks a session: any failure is
swallowed, it exits 0, and it returns within about 50 ms unless it is holding a
permission request for the map.

It also keeps the first payload of each event per Claude Code version in
`~/.colony/fixtures/<version>/` (unscrubbed; scrub before copying any into
`crates/colony-hook/tests/fixtures/`, which the contract tests read).

## Models

The New session dialog picks the model (`--model`). The inspector shows each
session's model and, for sessions Colony started, buttons to switch. Switching
restarts the session on the new model with `--resume`, so the conversation
carries over and your default model for new sessions doesn't change (typing
`/model` in a session would change it). It's offered between turns only.

Colony also suggests a model when recent work fits another one better: mostly
reading and searching on a big model (Haiku), planning on a lighter one
(Opus), or editing and running code on Haiku (Sonnet). Suggestions never
switch anything on their own. The rules are in `crates/colony-core/src/models.rs`.

## Token and cost tracking

Colony reads each session's transcript (`~/.claude/projects/**/*.jsonl`, on
Windows and inside each WSL distro through the probe) and counts the `usage` on
every assistant message: input, output, cache-read and cache-creation tokens,
per model. Lines are deduplicated on their uuid, a response written as several
lines counts once, a half-written last line waits for its newline, and a
transcript that grows while it is read is picked up where it left off.
Subagents' tokens roll up into their parent. Colony keeps only the numbers, not
transcript text.

Shown as **estimates** (the `~`): the top bar has everything spent today (since
local midnight), each district header has that project's total, and the
inspector has one Usage row per bot. Cost is tokens times list prices, in
`crates/colony-core/src/prices.rs` (with where the prices came from); it
ignores subscription plans, discounts, and 1-hour cache writes. A model with no
listed price still has its tokens counted but not its cost, and the UI says
**partial**. Today and project totals come from a 48-hour ledger the daemon
keeps in memory, so a restart rebuilds them from the transcripts.
`tools/synth` sends usage too (with an occasional unpriced model) for load tests.

## Where agents work

**Buildings.** Inside each project district, a building appears for each
directory working bots touch (Read, Edit, Write, Grep, Glob paths, relative to
the project folder and rolled up to two levels: `src/auth`). Working bots walk
to their building and a lit window shows for each one inside. A district holds
at most six; beyond that new directories fold into their top folder. A building
goes away five minutes after the last bot worked there. Bots with no file target
yet, or targets outside the project, roam the district as before.

**Edit collisions.** When two agents edit the same file within 2 minutes, or
different files in the same folder within 30 seconds, an orange warning sign
appears on the bots and the building, a dashed line joins the bots, and both
inspectors show an **Overlap** row. Only editing tools count (Edit, MultiEdit,
Write, NotebookEdit); reads never do, and a main agent and its own subagent are
never flagged against each other. It is only a warning: nothing is blocked and no
agent's state changes. It fades by itself once the editing stops. An edit counts once it has
succeeded (a denied or failed one never warns). Paths are compared as written, so
bots in separate worktrees of one repository are not compared with each other:
only bots sharing a checkout are.

**Changes on the dock.** For work ready to review, colonyd runs git (8 s timeout,
off the reducer's path, read-only, no index locks) in the session's folder,
through `wsl.exe` for WSL sessions, and shows `files · +added −removed` under the
package and in a **Changes** inspector row. It counts uncommitted and untracked
work, plus, for isolated sessions, commits since the branch left the default
branch. If git can't say (not a repository, timeout, more than 30 untracked
files) nothing is shown. Test daemons (`COLONY_INGEST=1`) show made-up counts.
`tools/synth` now emits file paths so all of this can be seen under load.

## Build the installer

    powershell -File scripts/build-installer.ps1

Builds the release binaries, the Linux `colony-probe` / `colony-hook`, and the
NSIS installer into `target/release/bundle/nsis/`. The Linux binaries are static
musl builds, taken from `-LinuxDir <folder>` (the CI artifact), or built inside a
WSL distro (`-Distro Ubuntu`, needs rustup there); `-SkipLinux` makes an
installer that can't set up WSL. The installer bundles `colonyd`, `colony-ptyd`,
`colony-hook`, `colony-setup` and the Linux files, as Tauri sidecars/resources
(`app/src-tauri/tauri.bundle.conf.json`; plain `cargo build` ignores it). Its
hooks are in `app/src-tauri/installer-hooks.nsh`: stop `colonyd` (not
`colony-ptyd`) before replacing files, and offer to remove Colony's hooks on
uninstall.

`scripts/build-app.ps1` and `scripts/update.ps1` still make the bare exe for
development. `scripts/install-probe.sh` still builds the probe inside a distro
with a Rust toolchain; use it when you are changing the probe.

To try Set up Colony without touching your real settings, point it at scratch
folders: `COLONY_SETUP_USER_HOME=<dir>` (stands in for `%USERPROFILE%` when
setting up Windows), `COLONY_SETUP_WSL_HOME=/tmp/x` (stands in for `$HOME` in
every distro), `COLONY_HOME=<dir>/.colony`, then start the app or run
`colony-setup`.

## Release and CI

`.github/workflows/ci.yml` runs `cargo test` on Windows and Linux (and the app's
`npm run build`) for every push and pull request. Pushing a tag `vX.Y.Z` (it
must match the version in `Cargo.toml` and `tauri.conf.json`) also builds the
static Linux probe and the installer and keeps them as the `colony-linux-x86_64`
and `colony-installer` artifacts. No secrets are used, so nothing is signed or
published.

TODO (needs the owner's decisions, see the pull request that added this):
automatic updates with the Tauri updater, code signing, and an aarch64 Linux
probe for WSL on ARM.

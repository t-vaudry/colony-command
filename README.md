# Colony Command

A desktop app that shows every Claude Code session (Windows terminals, WSL,
and the Claude desktop app) as a bot on a colony map, so you can see at a
glance which agents are working, blocked, waiting on you, or done.

Design spec: [`docs/design-spec.html`](docs/design-spec.html)

## Layout

| Path | What |
|---|---|
| `crates/colony-core` | Domain types and the state reducer (no I/O) |
| `crates/colony-source` | Watches the session registry and hook captures, emits events |
| `crates/colony-probe` | Runs in each WSL distro, streams its events to colonyd over stdio |
| `crates/colonyd` | Daemon: reducer + local WebSocket API on 127.0.0.1:7878 |
| `crates/colony-ptyd` | Terminal host: owns Colony's terminals so sessions survive daemon restarts |
| `app` | The map: PixiJS world + HUD, connects to colonyd |
| `app/src-tauri` | Desktop app: the map in a window; starts colonyd if it isn't running |
| `hooks/colony-approve.sh` | Approval hook: hands permission requests to colonyd for Allow/Deny on the map |
| `tools/synth`, `tools/replay` | Synthetic fleet generator and event-log replay, for load and visual tests |
| `spikes/capture` | Hook that records raw Claude Code hook payloads, for schema checks |
| `docs/` | Design spec |

Planned: `crates/colony-hook` (replaces the capture spike); approvals for WSL sessions.

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
which ends its sessions.

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

Register `hooks/colony-approve.sh` for Claude Code's `PermissionRequest` event
(copy it to `~/.colony/bin/`, then add to `~/.claude/settings.json`):

    { "type": "command", "command": "sh \"$HOME/.colony/bin/colony-approve.sh\"", "timeout": 600 }

While the map is open, permission requests show on the porch and in the
inspector with Allow, Always allow (adds Claude Code's suggested rule), and
Deny. The session's own prompt stays up too; whichever is answered first wins.
With colonyd stopped or no map open, the hook steps aside. Windows sessions
only for now: WSL hooks can't reach colonyd yet.

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

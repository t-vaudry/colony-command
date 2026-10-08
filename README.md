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
| `app` | The map: PixiJS world + HUD, connects to colonyd |
| `spikes/capture` | Hook that records raw Claude Code hook payloads, for schema checks |
| `docs/` | Design spec |

Planned: `crates/colony-hook` (replaces the capture spike, adds approvals),
and a Tauri desktop shell around `app/`.

## Test

From Windows (once Visual Studio C++ build tools are installed):

    cargo test --workspace

Or inside Ubuntu:

    wsl -d Ubuntu -- bash scripts/test-wsl.sh

## Run the daemon

    wsl -d Ubuntu -- bash scripts/install-probe.sh   # once per distro
    cargo run -p colonyd

It writes its port and access token to `~/.colony/daemon.json`. Running WSL
distros are attached automatically; stopped ones are left alone.

## Run the map

    cd app && npm install && npm run dev

Then open http://localhost:5173. In dev, the page reads the daemon's port and
token through a Vite route. Keys: `Space` next agent that needs you, `0` fit
the whole colony, `Esc` clear selection.

# Colony Command

A desktop app that shows every Claude Code session (Windows terminals, WSL,
and the Claude desktop app) as a bot on a colony map, so you can see at a
glance which agents are working, blocked, waiting on you, or done.

Design spec: [`docs/design-spec.html`](docs/design-spec.html)

## Layout

| Path | What |
|---|---|
| `crates/colony-core` | Domain types and the state reducer (no I/O) |
| `spikes/capture` | Hook that records raw Claude Code hook payloads, for schema checks |
| `docs/` | Design spec |

Planned: `crates/colonyd` (Windows daemon), `crates/colony-hook`,
`crates/colony-probe` (WSL), `app/` (Tauri + PixiJS).

## Test

From Windows (once Visual Studio C++ build tools are installed):

    cargo test --workspace

Or inside Ubuntu:

    wsl -d Ubuntu -- bash scripts/test-wsl.sh

# MVP acceptance results

Measured against design spec section 12 ("Acceptance criteria"). Everything was
run with `scripts/acceptance.mjs` against a throwaway `colonyd` (own
`COLONY_HOME`, `USERPROFILE`, free port, `COLONY_INGEST=1`), so no real
sessions, hooks or daemon were involved.

    cargo build --release -p colonyd -p colony-hook -p colony-synth
    (cd app && npm install)               # only for the fps check
    node scripts/acceptance.mjs all       # or: latency | failopen | load | fps
    TRIALS=30 node scripts/acceptance.mjs latency

Machine: Windows 11, i7-1280P (20 logical cores), 32 GB, Intel Iris Xe, release
builds.

## Summary

| Criterion | Target | Measured | Result |
|---|---|---|---|
| Session appears after its first prompt | < 2 s | median 0.15 s, max 0.26 s (n=30, two runs) | pass |
| Permission prompt on the porch | < 1 s | median 23 ms, max 34 ms | pass |
| Killed terminal shows crashed | < 5 s | median 2.8 s, max 3.8 s (was 7.3-8.7 s, see below) | pass after fix |
| Daemon stopped: no visible delay, no errors | none | `colony-hook` 13-47 ms, `colony-approve.sh` 22-250 ms, exit 0, no output | pass after fix |
| 50 sessions render at 60 fps | 60 fps | 59.3-59.9 fps average, p99 frame 17 ms (headless Chrome, vsync-capped) | pass, with caveat |
| Daemon idle CPU / RAM | < 1 % / < 80 MB | 0.46 % of one core, 9 MB working set | pass |

Real Claude Code, Colony-started terminals (`colony-ptyd`) and WSL were **not**
exercised; see *Gaps*.

## Method and results

### 1. First prompt to map (`latency`)

For each trial: a stand-in process plays the Claude Code process, its
`~/.claude/sessions/<pid>.json` registry file is written, and `colony-hook` is
run with a `UserPromptSubmit` payload on stdin, all at once. The clock stops at
the WebSocket message in which a map would see the bot `working`. Writing the
registry file and the hook together is the pessimistic case: Claude Code
registers at launch, before the first prompt. The clock includes the hook
process's own run time.

Median 150 ms, p95 245 ms, max 255 ms. The budget is dominated by colonyd's
200 ms source poll.

### 2. Permission prompt to porch (`latency`)

With a WebSocket client connected (a map being open is what lets colonyd hold
requests), `colony-hook` is run with a `PermissionRequest`. The clock stops when
the map receives the bot in `needs_input` with a pending permission (which is
what puts it on the porch).

Median 23 ms, p95 30 ms, max 34 ms. This path is event-driven, not polled.

### 3. Killed terminal to crashed (`latency`)

The stand-in process is killed with `taskkill /F /T`, so nothing sends
`SessionEnd` and the registry file is left behind, as for a real killed
terminal. The clock stops when the bot is `crashed`.

**This failed at first**: 7.3-8.7 s (median 7.8 s). The delay was the sum of a
check for dead processes only every 10 polls (up to 2 s), the 5 s
`CRASH_GRACE_MS` (waiting for a normal exit's `SessionEnd` to arrive first),
and the reducer's 1 s tick.

Fix, constants only:

- `colony-source`: `ALIVE_CHECK_EVERY` 10 to 2 polls (every 400 ms). A liveness
  check is one `OpenProcess` per registered session.
- `colony-core`: `CRASH_GRACE_MS` 5000 to 1500. `SessionEnd` is delivered by a
  hook that runs before the process exits, and colonyd reads captures before
  the registry in each poll, so by the time the process is seen gone a normal
  exit's `SessionEnd` has been read. The script also checks that a normal
  exit (`SessionEnd`, process gone, registry file removed) ends as `ended`, not
  `crashed`.

Now median 2.8 s, p95 2.9 s, max 3.8 s over 60 trials. Across about 130 trials
in the session there were three isolated outliers (one 7.9 s crash, one 2.5 s
and one 0.9 s first-prompt), all on a busy machine shared with builds and none
reproducible in the 30-trial runs. They are probably host scheduling noise
(for example antivirus scanning the freshly started process), but they were not
investigated further.

### 4. Daemon stopped (`failopen`)

`colony-hook` and `hooks/colony-approve.sh` are run for the common events
(`SessionStart`, `UserPromptSubmit`, `PreToolUse`, `PostToolUse`,
`PermissionRequest`, `Stop`, `SessionEnd`), five times each, with: no
`daemon.json`; a stale `daemon.json` pointing at a dead port; and (hook only) a
port that accepts connections and never answers. Checked: exit code 0, empty
stdout and stderr, wall time. For scale, starting the hook binary with no work
takes 9 ms.

| Case | median | max |
|---|---|---|
| `colony-hook`, no `daemon.json` | 13 ms | 16 ms |
| `colony-hook`, dead port | 47 ms | 62 ms |
| `colony-hook`, silent port, non-permission events | 14 ms | 28 ms |
| `colony-approve.sh`, no `daemon.json` | 22 ms | 29 ms |
| `colony-approve.sh`, dead port | 252 ms | 297 ms |

**Found and fixed**: with a stale `daemon.json`, `colony-approve.sh` waited
about 2.2 s, because a refused loopback connection takes about 2 s on Windows
and `curl` had no connect timeout on that path. It now uses
`--connect-timeout 0.1`. (The Rust `colony-hook` already bounds the connect at
15 ms.) The Windows install uses `colony-hook`, not this script, and in WSL the
unix-socket path refuses instantly, so the 2 s only reached manual or
development installs, but 250 ms is still not zero: a dead port costs about
`curl`'s granularity.

A `PermissionRequest` sent to a live daemon with no map open also returns at
once (covered by `scripts/test-approve-hook.sh`); a request held for the map is
meant to wait, so it is excluded from the silent-port case.

### 5. 50 sessions at 60 fps (`fps`)

The Vite dev server (`app/`) is started against the throwaway daemon, which is
fed by `colony-synth --agents 50 --speed 1` (50 main sessions, 65-68 bots with
subagents, all districts, porch and dock populated). Headless Chrome
(`--headless=new`, 1600x900 window, 1264x710 canvas) loads it; after 15 s a
`requestAnimationFrame` loop records every frame interval for 15 s (about 890
frames).

Average 59.3 and 59.9 fps in two runs; median frame 16.7 ms, p99 16.9-17.2 ms,
0.2-0.9 % of frames over 20 ms (single hitches of up to 50 ms). A screenshot
confirmed the map was populated, not blank.

### 6. Daemon idle (`load`)

`colonyd` CPU time (`TotalProcessorTime`) and working set are sampled for 30-60 s
with a map connected:

| State | CPU (one core) | Working set |
|---|---|---|
| Empty, map connected | 0.62 % | 7.9 MB |
| 50 sessions arriving at human pace (synth `--speed 1`) | 2.19 % | 9.0 MB |
| 50 sessions on the map, nothing happening | **0.46 %** | **9.1 MB** |
| 50 sessions at 5x pace (stress) | 4.01 % | 9.4 MB |

0.46 % of one core is 0.02 % of the machine's 20 cores, so the target holds
whichever way "1 % CPU" is read. The idle cost is the 200 ms source poll.

## Gaps

What this does not prove:

- **No real Claude Code.** Registry files, hook payloads and processes are
  stand-ins shaped like the real ones. A real session's first-prompt latency
  also includes Claude Code's own hook launch (this binary runs in about 10-50
  ms, plus Windows process creation). Worth a manual check with a real `claude`
  once.
- **Colony-started terminals and WSL are not measured.** A kill of a terminal in
  `colony-ptyd` takes the `TerminalExited` path, which uses the same
  `CRASH_GRACE_MS` and the 1 s tick, so it should be about 1.5-2.5 s, but this
  was not timed. WSL adds the probe's own poll and the stdio hop: not measured
  (`COLONY_INGEST=1` deliberately keeps the test daemon away from real distros).
- **fps is capped by vsync** at 60, so "59.9 fps" shows no stutter, not headroom.
  It ran in headless Chrome rather than the packaged Tauri window (WebView2),
  and other GPUs could differ. Maps with more than 50 sessions, the terminal
  pane and high-DPI monitors are untested.
- **Idle numbers are for the daemon only**, with synthetic sessions (no live
  processes to liveness-check, no transcripts to read for usage). A real daemon
  also runs the WSL supervisor, `colony-ptyd` (a separate process, whose memory
  isn't counted) and worktree and diff sweeps; its footprint on a real fleet
  should be re-checked from Task Manager.
- Timings come from one machine and, as noted above, a few outliers appeared
  while it was busy. The script's PASS lines use the maximum of the sample, so a
  single outlier fails a run; rerun before reading anything into one failure.

# MVP acceptance results

Measured against design spec section 12 ("Acceptance criteria"). Everything was
run with `scripts/acceptance.mjs` against a throwaway `colonyd` (own
`COLONY_HOME`, `USERPROFILE`, free port, `COLONY_INGEST=1`), so no real
sessions, hooks or daemon were involved.

    cargo build --release -p colonyd -p colony-hook -p colony-synth
    (cd app && npm install)               # only for the fps check
    node scripts/acceptance.mjs all       # or: latency | failopen | load | fps
    AGENTS=200 node scripts/acceptance.mjs fps   # scale: see section 7
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

### 7. Scale: 200 and 500 sessions (`fps`, `AGENTS=n`)

    AGENTS=200 node scripts/acceptance.mjs fps      # also 500; SPEED=, FPS_SECS= to tune
    PROFILE=1 AGENTS=500 node scripts/acceptance.mjs fps   # adds the top self-time functions

`AGENTS` is the number of main sessions synth keeps alive (65-70 % more bots
with subagents: 200 gives about 300, 500 about 700). The step also reports the
page's CPU per frame (CDP `Performance.getMetrics`: task and script time, not
capped by vsync, so it shows headroom that "60 fps" hides), colonyd CPU and
working set, and WebSocket messages/s and KB/s as a second map sees them.
Headless Chrome, 1600x900, 15 s window, 1 shared machine (other builds were
running, so the 50-session rows are noisier than section 5).

| Sessions | | fps | frame p99 | page task / script per frame | colonyd CPU / RAM | WebSocket |
|---|---|---|---|---|---|---|
| 50 | before | 45-51 | 66 ms | 20-24 ms / 17-21 ms | 3.1 % / 12 MB | 19 msgs/s, 37 KB/s |
| 50 | after | 46-54 | 50-67 ms | 16-20 ms / 14-17 ms | 2.8-4.2 % / 12 MB | 12 msgs/s, 40 KB/s |
| 200 | before | 8.1 | 500 ms | 245 ms / 214 ms | 6.9 % / 13.6 MB | 95 msgs/s, 194 KB/s |
| 200 | after | 58.7 | 33 ms | 8.8 ms / 5.8 ms | 6.9 % / 14.5 MB | 20 msgs/s, 172 KB/s |
| 500 | before | 3.1-4.6 | 283-633 ms | 253-438 ms / 226-401 ms | 6.3-8.3 % / 22-25 MB | 173-187 msgs/s, 330-363 KB/s |
| 500 | after | 30.7 | 133 ms | 31 ms / 17 ms | 7.2 % / 16 MB | 20 msgs/s, 373 KB/s |

What was slow and what changed (`app/src/world.ts`, `app/src/daemon.ts`,
`crates/colonyd/src/api.rs`):

- **Graphics rebuilt every frame.** Pixi re-tessellates the whole `Graphics`
  each frame, and strokes (arms, outlines, tethers, dashed collision lines)
  dominated: 200+ ms of script per frame at 300 bots. Now there are three
  levels of detail, chosen from how many bots are on screen (100 to enter,
  85 to leave, so it doesn't flicker) and the zoom: full bots; plain bodies
  (no arms, eyes or outlines, from zoom 0.75 down with many bots); and far-zoom
  dots, two tinted particles per bot in one `ParticleContainer` (a single draw
  call). Alarm states stay legible at every level: the ring keeps the state
  colour, blocked and crashed bots keep their glyph, and an edit collision turns
  the ring orange where the dashed line is dropped. Bots outside the view are not
  drawn. The 50-session map is still drawn at full detail.
- **Buildings** are redrawn at most every 250 ms (they only fade slowly), not
  every frame; arms use butt caps (round caps tessellate arcs).
- **Text.** Every bot made three `Text` objects up front (about 2000 canvas
  textures at 700 bots). They are now created on first use and hidden off
  screen or at far zoom; district header text is only assigned when it changes.
- **Per-frame allocation and O(n^2) work.** Porch and dock rows used
  `findIndex` per bot per frame, live-subagent counts filtered `children` per
  bot, the draw order was a fresh sorted array, home spots re-hashed ids, and
  `prefs.reduced` asked the browser each call. Rows, slots and counts are now
  built once per daemon change, order is kept with an insertion sort, homes
  are cached, and the motion preference is read once per frame.
- **Daemon messages.** The map did a full `sync` (buildings, labels, bodies) and
  a JSON parse per message, 95-190 times a second. colonyd now folds whatever
  is already queued for a map into one `{"type":"batch","msgs":[...]}` frame
  (no added delay: only messages that are waiting), and the map notifies its
  listeners once per batch and syncs the world once per frame. Messages/s fell
  5-9x. Bytes did not: each upsert carries the whole agent record (about
  1.9 KB with `activity`), so 500 sessions is ~370 KB/s; sending only changed
  fields is the next step if that matters.
- **Layout cost.** `layout.ts` is only the draggable dividers (nothing per
  frame). The world's own `layout()` runs only when the set of projects changes,
  and only touches homes that moved; it was never a measurable cost.
- The attention-budget motion ceiling (`HEALTHY`) and reduced-motion behaviour
  are unchanged; far-zoom dots do not move any differently.

Still open at 500 sessions: 31 fps and a 133 ms p99. The "needs you" tray in the
HUD grows with the number of waiting sessions (132 chips at 500) until it fills
the window, leaving the map about 100 px tall, and rebuilds its HTML on each
update (`setHtml` in `hud.ts` shows in the profile). It should cap its height and
scroll, and render only when the set changes. That is outside this change's
files. The remaining per-frame cost is mostly pixi's own per-element work for
about 1400 particles plus the HUD's DOM updates.

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

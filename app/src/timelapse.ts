import { STATE_LABEL, type AgentState, type Replay } from "./types";

const esc = (s: string) => s.replace(/[&<>"']/g, (c) => `&#${c.charCodeAt(0)};`);

const RANGES: [string, number][] = [
  ["Last hour", 60 * 60_000],
  ["Last 4 hours", 4 * 60 * 60_000],
  ["Last 24 hours", 24 * 60 * 60_000],
];

export const REPLAY_FRAMES = 240;

const COLOR: Record<AgentState, string> = {
  spawning: "var(--ok)",
  working: "var(--ok)",
  needs_input: "var(--input)",
  awaiting_reply: "var(--input)",
  blocked: "var(--crit)",
  crashed: "var(--crit)",
  ready_to_review: "var(--done)",
  idle: "var(--idle)",
  ended: "var(--idle)",
};

const W = 240;
const ROW = 9;

/** x of the playhead for frame `at`. */
export function playheadX(replay: Replay, at: number): number {
  return (Math.min(at, replay.frames.length - 1) / replay.frames.length) * W + W / replay.frames.length / 2;
}

/** Who was doing what at one frame: the part that changes as the scrubber moves. */
export function detail(replay: Replay, at: number): string {
  const f = replay.frames[Math.min(at, replay.frames.length - 1)];
  if (!f) return "";
  const time = new Date(f.ts).toLocaleString(undefined, { weekday: "short", hour: "numeric", minute: "2-digit" });
  const rows = f.agents
    .map(([, name, project, state]) => `<div class="row"><span><span class="dot" style="background:${COLOR[state]}"></span> <b>${esc(name)}</b> <span class="muted small">${esc(project ?? "")}</span></span><span class="muted small">${STATE_LABEL[state]}</span></div>`)
    .join("");
  return `<p><b>${esc(time)}</b></p>${rows || `<p class="muted small">No sessions yet at this point.</p>`}`;
}

/** The whole view: range buttons, then (once loaded) a lane per session with a playhead, a scrubber, and the detail. */
export function timelapse(replay: Replay | null, at: number, playing: boolean, loading: boolean): string {
  const buttons = RANGES.map(([label, ms]) => `<button type="button" data-replay="${ms}"${loading ? " disabled" : ""}>${label}</button>`).join("");
  const head = `<div class="k">Time-lapse</div><p class="muted small">Replays what Colony logged: who was working, waiting on you, or done. Logging starts when this version first runs, and keeps 48 hours.</p><div class="actions">${buttons}</div>`;
  if (loading) return `${head}<p class="muted small">Loading…</p>`;
  if (!replay) return head;
  const seen = new Map<string, { name: string; states: (AgentState | null)[] }>();
  replay.frames.forEach((f, i) => {
    for (const [id, name, , state] of f.agents) {
      const lane = seen.get(id) ?? { name, states: new Array(replay.frames.length).fill(null) };
      lane.states[i] = state;
      seen.set(id, lane);
    }
  });
  if (!seen.size) return `${head}<p class="muted small">Nothing was logged in that window.</p>`;
  const n = replay.frames.length;
  const bw = W / n;
  const lanes = [...seen.values()]
    .map((lane, row) => {
      const segs: string[] = [];
      for (let i = 0; i < n; ) {
        const s = lane.states[i];
        let j = i;
        while (j < n && lane.states[j] === s) j++;
        if (s) segs.push(`<rect x="${(i * bw).toFixed(2)}" y="${row * ROW}" width="${((j - i) * bw).toFixed(2)}" height="${ROW - 2}" fill="${COLOR[s]}"><title>${esc(lane.name)}: ${STATE_LABEL[s]}</title></rect>`);
        i = j;
      }
      return segs.join("");
    })
    .join("");
  const h = seen.size * ROW;
  const x = playheadX(replay, at).toFixed(2);
  return `${head}
    <svg id="tl-lanes" viewBox="0 0 ${W} ${h}" width="100%" height="${Math.min(h, 160)}" preserveAspectRatio="none" role="img" aria-label="One lane per session, coloured by what it was doing">${lanes}<line id="tl-head" x1="${x}" x2="${x}" y1="0" y2="${h}" stroke="var(--ink)" stroke-width="0.8"/></svg>
    <div class="actions"><button type="button" data-replay-play="1">${playing ? "Pause" : "Play"}</button>
      <input type="range" id="tl-scrub" data-replay-at="1" min="0" max="${n - 1}" value="${Math.min(at, n - 1)}" aria-label="Time" style="flex:1"></div>
    <div id="tl-detail">${detail(replay, at)}</div>`;
}

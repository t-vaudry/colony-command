// The Workflow block of the bot panel: the phases a session's workflow lists as
// a small graph, loops drawn around the runs of phases that repeat, the path
// the session has taken through them, and the subagents running in each phase.

import type { Agent, Workflow, WorkflowInfo, WorkflowLoop } from "./types";

const esc = (s: string) => s.replace(/[&<>"']/g, (c) => `&#${c.charCodeAt(0)};`);

const NODE_W = 92;
const NODE_H = 30;
const GAP_X = 34;
const GAP_Y = 26;
const PAD = 10;
/** Room under the graph for edges that go back to an earlier phase. */
const BACK_ROOM = 22;
/** A loop's frame sits this far outside its phases, and a further step more for each loop inside it. */
const FRAME = 6;
const FRAME_STEP = 6;
/** Room above a frame for its label. */
const LABEL_ROOM = 16;

export interface Node {
  skill: string;
  col: number;
  row: number;
  x: number;
  y: number;
}

/** Where phase `i` usually leads: its own `next`, else the phase listed after it. */
export function followers(w: Workflow, i: number): string[] {
  const p = w.phases[i];
  if (p.next.length) return p.next;
  return w.phases[i + 1] ? [w.phases[i + 1].skill] : [];
}

/**
 * Lay phases out in columns by how far they are from the first phase (the
 * shortest way there along where phases lead); phases nothing leads to follow
 * in order after the ones that are reached.
 */
export function layout(w: Workflow): Node[] {
  const depth = new Map<string, number>();
  const spread = (from: string, at: number) => {
    const queue = [from];
    depth.set(from, at);
    while (queue.length) {
      const s = queue.shift()!;
      const i = w.phases.findIndex((p) => p.skill === s);
      for (const n of i >= 0 ? followers(w, i) : []) {
        if (!depth.has(n)) {
          depth.set(n, depth.get(s)! + 1);
          queue.push(n);
        }
      }
    }
  };
  for (const p of w.phases) {
    // The first phase is column 0; one nothing leads to starts a column after what's placed.
    if (!depth.has(p.skill)) spread(p.skill, depth.size ? Math.max(...depth.values()) + 1 : 0);
  }
  const rows = new Map<number, number>();
  return w.phases.map((p) => {
    const col = depth.get(p.skill) ?? 0;
    const row = rows.get(col) ?? 0;
    rows.set(col, row + 1);
    return { skill: p.skill, col, row, x: col * (NODE_W + GAP_X), y: row * (NODE_H + GAP_Y) };
  });
}

/** Transitions the session actually made, as "from>to" pairs with how often. */
export function taken(info: WorkflowInfo): Map<string, number> {
  const out = new Map<string, number>();
  for (let i = 1; i < info.visits.length; i++) {
    const key = `${info.visits[i - 1].skill}>${info.visits[i].skill}`;
    out.set(key, (out.get(key) ?? 0) + 1);
  }
  return out;
}

/** The phases in the order visited: "plan → build → plan". */
export function pathLine(info: WorkflowInfo): string {
  return info.visits.map((v) => v.skill).join(" → ");
}

/** Subagents of `kids` that are still working in each phase. */
export function runningByPhase(kids: Agent[]): Map<string, number> {
  const out = new Map<string, number>();
  for (const k of kids) {
    if (!k.phase || k.state === "ended" || k.state === "crashed") continue;
    out.set(k.phase, (out.get(k.phase) ?? 0) + 1);
  }
  return out;
}

/** The index span of a loop in the phase list, or null if it names a phase that isn't there. */
function span(w: Workflow, l: WorkflowLoop): [number, number] | null {
  const a = w.phases.findIndex((p) => p.skill === l.from);
  const b = w.phases.findIndex((p) => p.skill === l.to);
  return a < 0 || b < a ? null : [a, b];
}

/**
 * Which round of a loop the session is in: 1 when it enters the run of phases,
 * one more each time it goes back to an earlier phase of the run. 0 if it never
 * entered. `inside` says whether the session is in the run now.
 */
export function loopRound(info: WorkflowInfo, l: WorkflowLoop): { round: number; inside: boolean } {
  const sp = span(info.workflow, l);
  if (!sp) return { round: 0, inside: false };
  const index = (skill: string) => info.workflow.phases.findIndex((p) => p.skill === skill);
  let round = 0;
  let prev = -1;
  for (const v of info.visits) {
    const i = index(v.skill);
    const within = i >= sp[0] && i <= sp[1];
    if (within) round = prev < 0 ? 1 : i <= prev ? round + 1 : round;
    prev = within ? i : -1;
  }
  const last = info.active ? index(info.active) : -1;
  return { round, inside: last >= sp[0] && last <= sp[1] };
}

/** How many loops sit inside each loop (0 for an innermost one), to space the frames. */
function nesting(w: Workflow): number[] {
  const spans = w.loops?.map((l) => span(w, l)) ?? [];
  const level = (i: number): number => {
    const s = spans[i];
    if (!s) return 0;
    let deepest = -1;
    spans.forEach((t, j) => {
      if (j !== i && t && s[0] <= t[0] && t[1] <= s[1]) deepest = Math.max(deepest, level(j));
    });
    return deepest + 1;
  };
  return spans.map((_, i) => level(i));
}

function graph(owner: string, info: WorkflowInfo, kids: Agent[]): string {
  // Marker ids are per session: graphs of several sessions can be in the page, and the same one must redraw identically.
  const ids = { arrow: `wf-arrow-${owner.replace(/[^A-Za-z0-9_-]/g, "-")}` };
  const w = info.workflow;
  const nodes = layout(w);
  const at = new Map(nodes.map((n) => [n.skill, n]));
  const cols = Math.max(...nodes.map((n) => n.col)) + 1;
  const rows = Math.max(...nodes.map((n) => n.row)) + 1;
  const loops = (w.loops ?? []).filter((l) => span(w, l));
  const levels = nesting({ ...w, loops });
  const outer = loops.length ? FRAME + FRAME_STEP * Math.max(...levels) : 0;
  const hasBack = w.phases.some((_, i) => followers(w, i).some((n) => (at.get(n)?.col ?? 0) <= (at.get(w.phases[i].skill)?.col ?? 0))) || loops.length > 0;
  const innerW = cols * NODE_W + (cols - 1) * GAP_X;
  const innerH = rows * NODE_H + (rows - 1) * GAP_Y + (hasBack ? BACK_ROOM : 0);
  const left = PAD + outer;
  const top = PAD + (loops.length ? outer + LABEL_ROOM : 0);
  const width = left * 2 + innerW;
  const height = top + innerH + PAD + outer;
  const walked = taken(info);
  const visits = new Map<string, number>();
  for (const v of info.visits) visits.set(v.skill, (visits.get(v.skill) ?? 0) + 1);
  const running = runningByPhase(kids);
  const arrow = (used: boolean) => `url(#${ids.arrow}${used ? "-on" : ""})`;

  const edges: string[] = [];
  const drawn = new Set<string>();
  const back = (a: Node, b: Node, used: boolean, extra = "") => {
    // Back to an earlier phase, sideways, or to itself: loop underneath.
    const same = a === b;
    const x1 = a.x + NODE_W / 2 + (same ? 14 : 0);
    const x2 = b.x + NODE_W / 2 - (same ? 14 : 0);
    const y = Math.max(a.y, b.y) + NODE_H;
    const dip = y + BACK_ROOM - 4;
    edges.push(`<path class="wf-edge back${used ? " taken" : ""}${extra}" d="M${x1} ${y} C${x1 + (same ? 8 : 0)} ${dip} ${x2 - (same ? 8 : 0)} ${dip} ${x2} ${y + 3}" marker-end="${arrow(used)}"/>`);
  };
  w.phases.forEach((p, i) => {
    const a = at.get(p.skill)!;
    for (const n of followers(w, i)) {
      const b = at.get(n);
      if (!b) continue;
      drawn.add(`${p.skill}>${n}`);
      const used = walked.has(`${p.skill}>${n}`);
      if (b.col > a.col) {
        const x1 = a.x + NODE_W;
        const y1 = a.y + NODE_H / 2;
        const x2 = b.x;
        const y2 = b.y + NODE_H / 2;
        const mx = (x1 + x2) / 2;
        edges.push(`<path class="wf-edge${used ? " taken" : ""}" d="M${x1} ${y1} C${mx} ${y1} ${mx} ${y2} ${x2 - 3} ${y2}" marker-end="${arrow(used)}"/>`);
      } else {
        back(a, b, used);
      }
    }
  });
  // The way back round a loop, unless a phase already leads there.
  for (const l of loops) {
    const a = at.get(l.to)!;
    const b = at.get(l.from)!;
    if (!drawn.has(`${l.to}>${l.from}`)) {
      drawn.add(`${l.to}>${l.from}`);
      back(a, b, walked.has(`${l.to}>${l.from}`), " loop-back");
    }
  }

  const frames = loops.map((l, k) => {
    const sp = span(w, l)!;
    const members = w.phases.slice(sp[0], sp[1] + 1).map((p) => at.get(p.skill)!);
    const pad = FRAME + FRAME_STEP * levels[k];
    const x1 = Math.min(...members.map((n) => n.x)) - pad;
    const y1 = Math.min(...members.map((n) => n.y)) - pad;
    const x2 = Math.max(...members.map((n) => n.x + NODE_W)) + pad;
    const y2 = Math.max(...members.map((n) => n.y + NODE_H)) + pad;
    const { round, inside } = loopRound(info, l);
    const until = l.until ? (l.until.length > 30 ? `${l.until.slice(0, 29)}…` : l.until) : "";
    const label = `↻ ${round ? `round ${round} of ${l.max}` : `up to ${l.max} rounds`}${until ? ` · until ${until}` : ""}`;
    return `<g class="wf-loop${inside ? " active" : round ? " done" : ""}"><title>${esc(`Repeats ${l.from} to ${l.to}${l.until ? ` until ${l.until}` : ""}, at most ${l.max} rounds`)}</title><rect x="${x1}" y="${y1}" width="${x2 - x1}" height="${y2 - y1}" rx="${10 + 3 * levels[k]}"/><text class="wf-loop-label" x="${x1 + 6}" y="${y1 - 4}">${esc(label)}</text></g>`;
  });

  const boxes = nodes.map((n, i) => {
    const n_visits = visits.get(n.skill) ?? 0;
    const custom = !!w.phases[i].instruction;
    const state = (info.active === n.skill ? " active" : n_visits ? " done" : "") + (custom ? " custom" : "");
    const count = n_visits > 1 ? `<text class="wf-count" x="${n.x + NODE_W - 5}" y="${n.y + 11}" text-anchor="end">×${n_visits}</text>` : "";
    const kind = custom ? `<text class="wf-kind" x="${n.x + 6}" y="${n.y + 11}">✎</text>` : "";
    const busy = running.get(n.skill);
    const dots = busy ? `<text class="wf-subs" x="${n.x + NODE_W / 2}" y="${n.y + NODE_H - 5}" text-anchor="middle">${busy} subagent${busy === 1 ? "" : "s"}</text>` : "";
    const label = n.skill.length > 14 ? `${n.skill.slice(0, 13)}…` : n.skill;
    return `<g class="wf-node${state}"><title>${esc(n.skill)}${custom ? " (custom phase)" : ""}${n_visits ? ` · visited ${n_visits}×` : ""}</title><rect x="${n.x}" y="${n.y}" width="${NODE_W}" height="${NODE_H}" rx="6"/><text class="wf-name" x="${n.x + NODE_W / 2}" y="${n.y + (busy ? 13 : 19)}" text-anchor="middle">${esc(label)}</text>${kind}${count}${dots}</g>`;
  });

  return `<svg class="wf-graph" viewBox="0 0 ${width} ${height}" width="${width}" height="${height}" role="img" aria-label="Workflow phases">
    <defs>
      <marker id="${ids.arrow}" viewBox="0 0 8 8" refX="7" refY="4" markerWidth="6" markerHeight="6" orient="auto"><path d="M0 0 L8 4 L0 8 z" class="wf-head"/></marker>
      <marker id="${ids.arrow}-on" viewBox="0 0 8 8" refX="7" refY="4" markerWidth="6" markerHeight="6" orient="auto"><path d="M0 0 L8 4 L0 8 z" class="wf-head on"/></marker>
    </defs>
    <g transform="translate(${left} ${top})">${frames.join("")}${edges.join("")}${boxes.join("")}</g>
  </svg>`;
}

/** The Workflow block; empty for a bot that wasn't started with one. `kids` are its subagents. */
export function workflowBlock(a: Agent, kids: Agent[]): string {
  const info = a.workflow;
  if (a.kind !== "main" || !info || info.workflow.phases.length === 0) return "";
  const path = pathLine(info);
  const status = info.active ? `In <b>${esc(info.active)}</b>` : "No phase started yet";
  return `<div class="fact workflow-block">
    <span class="k">Workflow · ${esc(info.workflow.label)}</span>
    <div class="wf-status muted small">${status}${info.visits.length > 1 ? ` · ${info.visits.length} phases so far` : ""}</div>
    <div class="wf-scroll">${graph(a.id, info, kids)}</div>
    ${path ? `<div class="wf-path small">${esc(path)}</div>` : ""}
  </div>`;
}

/** A subagent's phase, for its panel: "Started in phase build", or "". */
export function phaseLine(a: Agent): string {
  // Not escaped: `fact` in the panel escapes its value.
  return a.kind === "subagent" && a.phase ? `Started in phase ${a.phase}` : "";
}

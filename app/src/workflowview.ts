// The Workflow block of the bot panel: the phases a session's workflow lists as
// a small graph, the path the session has taken through them, and the
// subagents running in each phase.

import type { Agent, Workflow, WorkflowInfo } from "./types";

const esc = (s: string) => s.replace(/[&<>"']/g, (c) => `&#${c.charCodeAt(0)};`);

const NODE_W = 92;
const NODE_H = 30;
const GAP_X = 34;
const GAP_Y = 26;
const PAD = 10;
/** Room under the graph for edges that go back to an earlier phase. */
const BACK_ROOM = 22;

export interface Node {
  skill: string;
  col: number;
  row: number;
  x: number;
  y: number;
}

/**
 * Lay phases out in columns by how far they are from the first phase (the
 * shortest way there along `next`); phases nothing leads to follow in order
 * after the ones that are reached.
 */
export function layout(w: Workflow): Node[] {
  const depth = new Map<string, number>();
  const spread = (from: string, at: number) => {
    const queue = [from];
    depth.set(from, at);
    while (queue.length) {
      const s = queue.shift()!;
      for (const n of w.phases.find((p) => p.skill === s)?.next ?? []) {
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
    return { skill: p.skill, col, row, x: PAD + col * (NODE_W + GAP_X), y: PAD + row * (NODE_H + GAP_Y) };
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

/** Marker ids are per session: graphs of several sessions can be in the page, and the same one must redraw identically. */


function graph(owner: string, info: WorkflowInfo, kids: Agent[]): string {
  const ids = { arrow: `wf-arrow-${owner.replace(/[^A-Za-z0-9_-]/g, "-")}` };
  const w = info.workflow;
  const nodes = layout(w);
  const at = new Map(nodes.map((n) => [n.skill, n]));
  const cols = Math.max(...nodes.map((n) => n.col)) + 1;
  const rows = Math.max(...nodes.map((n) => n.row)) + 1;
  const hasBack = w.phases.some((p) => p.next.some((n) => (at.get(n)?.col ?? 0) <= (at.get(p.skill)?.col ?? 0)));
  const width = PAD * 2 + cols * NODE_W + (cols - 1) * GAP_X;
  const height = PAD * 2 + rows * NODE_H + (rows - 1) * GAP_Y + (hasBack ? BACK_ROOM : 0);
  const walked = taken(info);
  const visits = new Map<string, number>();
  for (const v of info.visits) visits.set(v.skill, (visits.get(v.skill) ?? 0) + 1);
  const running = runningByPhase(kids);

  const edges: string[] = [];
  for (const p of w.phases) {
    const a = at.get(p.skill)!;
    for (const n of p.next) {
      const b = at.get(n);
      if (!b) continue;
      const used = walked.has(`${p.skill}>${n}`);
      const cls = `wf-edge${used ? " taken" : ""}`;
      if (b.col > a.col) {
        const x1 = a.x + NODE_W;
        const y1 = a.y + NODE_H / 2;
        const x2 = b.x;
        const y2 = b.y + NODE_H / 2;
        const mx = (x1 + x2) / 2;
        edges.push(`<path class="${cls}" d="M${x1} ${y1} C${mx} ${y1} ${mx} ${y2} ${x2 - 3} ${y2}" marker-end="url(#${ids.arrow}${used ? "-on" : ""})"/>`);
      } else {
        // Back to an earlier phase, sideways, or to itself: loop underneath.
        const same = a === b;
        const x1 = a.x + NODE_W / 2 + (same ? 14 : 0);
        const x2 = b.x + NODE_W / 2 - (same ? 14 : 0);
        const y = Math.max(a.y, b.y) + NODE_H;
        const dip = y + BACK_ROOM - 4;
        edges.push(`<path class="${cls} back" d="M${x1} ${y} C${x1 + (same ? 8 : 0)} ${dip} ${x2 - (same ? 8 : 0)} ${dip} ${x2} ${y + 3}" marker-end="url(#${ids.arrow}${used ? "-on" : ""})"/>`);
      }
    }
  }

  const boxes = nodes.map((n) => {
    const n_visits = visits.get(n.skill) ?? 0;
    const state = info.active === n.skill ? " active" : n_visits ? " done" : "";
    const count = n_visits > 1 ? `<text class="wf-count" x="${n.x + NODE_W - 5}" y="${n.y + 11}" text-anchor="end">×${n_visits}</text>` : "";
    const busy = running.get(n.skill);
    const dots = busy ? `<text class="wf-subs" x="${n.x + NODE_W / 2}" y="${n.y + NODE_H - 5}" text-anchor="middle">${busy} subagent${busy === 1 ? "" : "s"}</text>` : "";
    const label = n.skill.length > 14 ? `${n.skill.slice(0, 13)}…` : n.skill;
    return `<g class="wf-node${state}"><title>${esc(n.skill)}${n_visits ? ` · visited ${n_visits}×` : ""}</title><rect x="${n.x}" y="${n.y}" width="${NODE_W}" height="${NODE_H}" rx="6"/><text class="wf-name" x="${n.x + NODE_W / 2}" y="${n.y + (busy ? 13 : 19)}" text-anchor="middle">${esc(label)}</text>${count}${dots}</g>`;
  });

  return `<svg class="wf-graph" viewBox="0 0 ${width} ${height}" width="${width}" height="${height}" role="img" aria-label="Workflow phases">
    <defs>
      <marker id="${ids.arrow}" viewBox="0 0 8 8" refX="7" refY="4" markerWidth="6" markerHeight="6" orient="auto"><path d="M0 0 L8 4 L0 8 z" class="wf-head"/></marker>
      <marker id="${ids.arrow}-on" viewBox="0 0 8 8" refX="7" refY="4" markerWidth="6" markerHeight="6" orient="auto"><path d="M0 0 L8 4 L0 8 z" class="wf-head on"/></marker>
    </defs>
    ${edges.join("")}${boxes.join("")}
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

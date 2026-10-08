// DOM layer: state counts, the porch strip, and the inspector panel.

import type { Daemon } from "./daemon";
import { severity, STATE_LABEL, type Agent, type AgentState } from "./types";

const esc = (s: string) => s.replace(/[&<>"']/g, (c) => `&#${c.charCodeAt(0)};`);

function ago(ms: number): string {
  const s = Math.max(0, Math.round(ms / 1000));
  if (s < 60) return `${s}s`;
  const m = Math.round(s / 60);
  if (m < 60) return `${m}m`;
  const h = Math.floor(m / 60);
  return `${h}h ${m % 60}m`;
}

/** A duration that counts up in place without re-rendering its container. */
const since = (ts: number) => `<span data-since="${ts}"></span>`;

/** Replace an element's HTML only when it changed, so clicks are never lost
 *  to a re-render between mousedown and mouseup. */
function setHtml(el: HTMLElement, html: string): void {
  if (el.dataset.html !== html) {
    el.innerHTML = html;
    el.dataset.html = html;
  }
}

function hostLabel(host: string): string {
  return host === "win" ? "Windows" : host.replace("wsl:", "WSL · ");
}

function originLabel(a: Agent): string {
  switch (a.entrypoint) {
    case "claude-desktop":
      return "Claude desktop app";
    case "cli":
      return "terminal";
    case null:
      return a.hooks_seen ? "hooks only" : "unknown";
    default:
      return a.entrypoint;
  }
}

const REASON_LABEL: Partial<Record<AgentState, string>> = {
  needs_input: "Permission",
  awaiting_reply: "Asks",
  blocked: "Problem",
  crashed: "Problem",
  ready_to_review: "Result",
};

export class Hud {
  selected: string | null = null;
  private counts = document.getElementById("counts")!;
  private conn = document.getElementById("conn")!;
  private strip = document.getElementById("porch-strip")!;
  private panel = document.getElementById("inspector")!;
  private scheduled = false;

  constructor(
    private daemon: Daemon,
    private onSelect: (id: string | null) => void,
  ) {
    daemon.onChange(() => this.schedule());
    setInterval(() => this.tickDurations(), 1000);
    this.strip.addEventListener("click", (e) => {
      const id = (e.target as HTMLElement).closest<HTMLElement>("[data-id]")?.dataset.id;
      if (id) this.onSelect(id);
    });
    this.panel.addEventListener("click", (e) => void this.action(e));
  }

  schedule(): void {
    if (this.scheduled) return;
    this.scheduled = true;
    requestAnimationFrame(() => {
      this.scheduled = false;
      this.render();
    });
  }

  private render(): void {
    const all = [...this.daemon.agents.values()];
    const count = (...s: AgentState[]) => all.filter((a) => s.includes(a.state)).length;
    setHtml(this.counts, [
      ["crit", "blocked", count("blocked", "crashed")],
      ["input", "needs you", count("needs_input", "awaiting_reply")],
      ["ok", "working", count("working")],
      ["done", "to review", count("ready_to_review")],
      ["idle", "idle", count("idle", "spawning")],
    ]
      .map(([c, label, n]) => `<span class="chip"><span class="dot ${c}"></span>${label} <b>${n}</b></span>`)
      .join(""));
    this.conn.className = `conn ${this.daemon.status}`;
    this.conn.textContent =
      this.daemon.status === "live"
        ? `live · ${all.filter((a) => a.kind === "main").length} sessions`
        : this.daemon.status === "connecting"
          ? "reconnecting…"
          : `offline: ${this.daemon.error ?? ""}`;

    const queue = all
      .filter((a) => severity(a.state) === "critical" || severity(a.state) === "input")
      .sort((a, b) => (severity(a.state) === "critical" ? 0 : 1) - (severity(b.state) === "critical" ? 0 : 1) || a.state_since - b.state_since);
    setHtml(this.strip, queue.length
      ? `<span class="strip-label">Needs you</span>` +
        queue
          .map(
            (a) =>
              `<button type="button" class="porch-item ${severity(a.state)}${a.id === this.selected ? " sel" : ""}" data-id="${esc(a.id)}">` +
              `<b>${esc(a.name)}</b> ${esc(STATE_LABEL[a.state])} · ${since(a.state_since)}</button>`,
          )
          .join("") +
        `<span class="hint">Space: next</span>`
      : `<span class="strip-label calm">Nothing needs you right now.</span>`);

    this.renderPanel();
    this.tickDurations();
  }

  private tickDurations(): void {
    const now = this.daemon.now();
    for (const el of document.querySelectorAll<HTMLElement>("[data-since]")) {
      el.textContent = ago(now - Number(el.dataset.since));
    }
  }

  private renderPanel(): void {
    const a = this.selected ? this.daemon.agents.get(this.selected) : undefined;
    if (!a) {
      setHtml(this.panel, `<div class="k">Inspector</div><h2>Click a bot</h2>
        <p class="muted">Bots on the front porch need you: red ones are stuck, yellow ones want a permission or an answer.
        Bots holding a blue package at the review dock have finished a turn.</p>
        <p class="muted">Drag to pan, scroll to zoom, double-click a bot to zoom to its project, <kbd>0</kbd> to fit everything.</p>`);
      return;
    }
    const parent = a.parent_id ? this.daemon.agents.get(a.parent_id) : undefined;
    const kids = a.children.map((c) => this.daemon.agents.get(c)).filter((k): k is Agent => !!k);
    const sev = severity(a.state);
    const row = (k: string, v: string | null | undefined, cls = "") =>
      v ? `<div class="row"><span class="k">${k}</span><span class="${cls}">${esc(v)}</span></div>` : "";
    const target = a.current_tool?.target?.replace(/\s+/g, " ");
    const short = target && target.length > 110 ? `${target.slice(0, 109)}…` : target;
    const toolRow = a.current_tool
      ? `<div class="row"><span class="k">Now</span><span>${esc(`${a.current_tool.name}${short ? `: ${short}` : ""}`)} (${since(a.current_tool.started_at)})</span></div>`
      : "";
    const resume = `claude --resume ${a.session_id}`;
    setHtml(this.panel, `
      <div class="k">${a.kind === "subagent" ? `Subagent of ${esc(parent?.name ?? "?")}` : esc(a.project_name ?? "unknown project")}</div>
      <h2>${esc(a.name)}</h2>
      <div><span class="pill ${sev ?? a.state}">${esc(STATE_LABEL[a.state])}</span> <span class="muted">for ${since(a.state_since)}</span></div>
      ${row(REASON_LABEL[a.state] ?? "Note", a.reason, "reason")}
      ${row("Session", a.title)}
      ${row("Objective", a.objective)}
      ${a.last_prompt !== a.objective ? row("Last prompt", a.last_prompt) : ""}
      ${toolRow}
      ${row("Where", `${hostLabel(a.host)} · ${originLabel(a)}${a.pid ? ` · pid ${a.pid}` : ""}`)}
      ${row("Folder", a.cwd, "mono")}
      ${row("Tool calls", a.tool_calls ? String(a.tool_calls) : null)}
      ${
        kids.length
          ? `<div class="row"><span class="k">Subagents</span><div class="kids">${kids
              .map((k) => `<button type="button" class="kid" data-select="${esc(k.id)}"><span class="dot ${severity(k.state) ?? (k.state === "working" ? "ok" : "idle")}"></span>${esc(k.name)} · ${esc(STATE_LABEL[k.state])}</button>`)
              .join("")}</div></div>`
          : ""
      }
      ${row("Last message", a.last_message)}
      ${!a.hooks_seen ? `<p class="muted small">Seen through the session registry only, so state is coarse. New sessions report full detail through hooks.</p>` : ""}
      <div class="actions">
        ${a.state === "ready_to_review" ? `<button type="button" data-ack="${esc(a.id)}">Mark reviewed</button>` : ""}
        ${a.state === "crashed" ? `<button type="button" data-ack="${esc(a.id)}">Clear</button>` : ""}
        ${a.kind === "main" ? `<button type="button" data-copy="${esc(resume)}">Copy resume command</button>` : ""}
      </div>
      ${
        a.kind === "main"
          ? `<p class="muted small">Colony can watch this session but can't type into it yet. ${
              a.state === "ready_to_review" ? "Mark reviewed only moves it off the dock. " : ""
            }To continue it, reply in ${a.entrypoint === "claude-desktop" ? "the Claude desktop app" : "its terminal"}.</p>`
          : ""
      }
      ${a.kind === "main" ? `<div class="mono small muted">${esc(resume)}</div>` : ""}`);
  }

  private async action(e: Event): Promise<void> {
    const el = (e.target as HTMLElement).closest<HTMLElement>("button");
    if (!el) return;
    if (el.dataset.select) this.onSelect(el.dataset.select);
    if (el.dataset.ack && !this.daemon.ack(el.dataset.ack)) el.textContent = "Not connected; try again";
    if (el.dataset.copy) {
      try {
        await navigator.clipboard.writeText(el.dataset.copy);
        el.textContent = "Copied";
      } catch {
        el.textContent = "Copy failed; select the text below";
      }
    }
  }
}

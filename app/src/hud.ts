// DOM layer: state counts, the porch strip, the inspector panel, and the
// reply box for sessions Colony started.

import type { Daemon } from "./daemon";
import { answersFrom, askCard, needsWide, questionsOf, type Pick } from "./ask";
import { repoDir, resumeWarning, worktreeName, type Prefill } from "./dialog";
import { MODELS, modelLabel, severity, STATE_LABEL, type Agent, type AgentState, type PermissionChoice } from "./types";

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
    // Keep the caret in an answer field the user is typing in.
    const f = document.activeElement as HTMLInputElement | null;
    const typing = f && el.contains(f) && f.dataset.other ? { q: f.dataset.other, at: f.selectionStart } : null;
    el.innerHTML = html;
    if (typing) {
      const n = el.querySelector<HTMLInputElement>(`[data-other="${typing.q}"]`);
      n?.focus();
      if (typing.at != null) n?.setSelectionRange(typing.at, typing.at);
    }
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
    case "colony":
      return "started by Colony";
    case null:
      return a.hooks_seen ? "hooks only" : "unknown";
    default:
      return a.entrypoint;
  }
}

/** Terminal key sequences for the prompt buttons. */
const KEYS: Record<string, string> = { up: "\x1b[A", down: "\x1b[B", enter: "\r", esc: "\x1b" };

const REASON_LABEL: Partial<Record<AgentState, string>> = {
  needs_input: "Permission",
  awaiting_reply: "Asks",
  blocked: "Problem",
  crashed: "Problem",
  ready_to_review: "Result",
};

export interface HudActions {
  select: (id: string | null) => void;
  showTerminal: (a: Agent) => void;
  newSession: (prefill?: Prefill) => void;
}

/** Two-step confirm: the first click arms the button for a few seconds. */
function confirmed(el: HTMLElement, prompt: string): boolean {
  if (el.dataset.armed) return true;
  const label = el.textContent;
  el.dataset.armed = "1";
  el.textContent = prompt;
  setTimeout(() => {
    delete el.dataset.armed;
    el.textContent = label;
  }, 4000);
  return false;
}

export class Hud {
  selected: string | null = null;
  private counts = document.getElementById("counts")!;
  private conn = document.getElementById("conn")!;
  private strip = document.getElementById("porch-strip")!;
  private panel = document.getElementById("insp-body")!;
  private compose = document.getElementById("compose")!;
  private reply = document.getElementById("reply") as HTMLTextAreaElement;
  private composeTarget: string | null = null;
  private drafts = new Map<string, string>();
  /** Agent whose "still running elsewhere" choice is showing in the panel. */
  private moveChoice: string | null = null;
  /** Model hints set aside with "Not now", by agent, until a different hint comes up. */
  private hintsDismissed = new Map<string, string>();
  /** Answers chosen so far for held questions, by request id. */
  private picks = new Map<string, Pick[]>();
  private scheduled = false;

  constructor(
    private daemon: Daemon,
    private actions: HudActions,
  ) {
    daemon.onChange(() => this.schedule());
    daemon.onError((m) => this.toast(m));
    setInterval(() => this.tickDurations(), 1000);
    this.strip.addEventListener("click", (e) => {
      const el = (e.target as HTMLElement).closest<HTMLElement>("button");
      if (el?.dataset.decide && el.dataset.req) {
        this.decide(el, el.dataset.req, el.dataset.decide as PermissionChoice);
        return;
      }
      const id = el?.dataset.id;
      if (id) this.actions.select(id);
    });
    this.panel.addEventListener("click", (e) => void this.action(e));
    this.panel.addEventListener("input", (e) => {
      const el = e.target as HTMLInputElement;
      if (!el.dataset.other || !el.dataset.req) return;
      this.pickFor(el.dataset.req, Number(el.dataset.other)).other = el.value;
      // Only the send button's enabled state changes, so the field keeps focus.
      const btn = this.panel.querySelector<HTMLButtonElement>("[data-answer]");
      const a = this.selected ? this.daemon.agents.get(this.selected) : undefined;
      if (btn && a) btn.disabled = !answersFrom(questionsOf(a), this.picks.get(el.dataset.req) ?? []);
    });
    this.reply.addEventListener("input", () => {
      if (this.composeTarget) this.drafts.set(this.composeTarget, this.reply.value);
    });
    this.reply.addEventListener("keydown", (e) => {
      if (e.key === "Enter" && !e.shiftKey) {
        e.preventDefault();
        this.sendReply();
      }
    });
    document.getElementById("send")!.addEventListener("click", () => this.sendReply());
    document.getElementById("interrupt")!.addEventListener("click", () => {
      const a = this.composeAgent();
      if (a?.terminal) this.daemon.interrupt(a.terminal);
    });
    document.getElementById("end")!.addEventListener("click", (e) => {
      const a = this.composeAgent();
      if (a?.terminal && confirmed(e.currentTarget as HTMLElement, "Click again to end the session")) this.daemon.kill(a.terminal);
    });
    document.getElementById("open-term")!.addEventListener("click", () => {
      const a = this.composeAgent();
      if (a) this.actions.showTerminal(a);
    });
    // Prompt keys and Allow/Deny press keys in the session, so menus can be
    // answered without the terminal pane having keyboard focus.
    this.compose.addEventListener("click", (e) => {
      const key = (e.target as HTMLElement).closest<HTMLElement>("[data-key]")?.dataset.key;
      if (key) this.press(KEYS[key]);
    });
    document.getElementById("perm-allow")!.addEventListener("click", () => this.press(KEYS.enter));
    document.getElementById("perm-deny")!.addEventListener("click", () => this.press(KEYS.esc));
  }

  private pickFor(req: string, qi: number): Pick {
    const list = this.picks.get(req) ?? [];
    this.picks.set(req, list);
    return (list[qi] ??= { labels: [], other: "" });
  }

  private press(seq: string | undefined): void {
    const a = this.composeAgent();
    if (!seq || !a?.terminal) return;
    if (!this.daemon.input(a.terminal, seq)) this.toast("Not connected to colonyd.");
  }

  toast(message: string): void {
    const t = document.getElementById("toast")!;
    t.textContent = message;
    t.hidden = false;
    clearTimeout(Number(t.dataset.timer));
    t.dataset.timer = String(setTimeout(() => (t.hidden = true), 6000));
  }

  private decide(el: HTMLElement, requestId: string, choice: PermissionChoice): void {
    if (!this.daemon.decide(requestId, choice)) {
      this.toast("Not connected to colonyd.");
      return;
    }
    el.closest(".actions")?.querySelectorAll("button").forEach((b) => b.setAttribute("disabled", ""));
  }

  /** The bot's model, a switcher for sessions Colony started, and any hint. */
  private modelRows(a: Agent): string {
    if (a.kind !== "main") return "";
    const current = modelLabel(a.model);
    // Switching restarts the session on the new model (the conversation
    // carries over), so not while Claude is in the middle of something.
    const busy = a.state === "working";
    const why = busy
      ? "Claude is mid-task; switch when this turn ends"
      : "Restarts this session on that model. The conversation carries over and your default model doesn't change.";
    const off = busy ? " disabled" : "";
    const switcher = a.terminal
      ? `<div class="model-switch">${MODELS.filter(([v]) => modelLabel(v)?.split(" ")[0] !== current?.split(" ")[0])
          .map(([v, label]) => `<button type="button" data-switch-model="${esc(v)}" data-agent="${esc(a.id)}" title="${esc(why)}"${off}>${esc(label)}</button>`)
          .join("")}</div>`
      : "";
    const modelRow = current || a.terminal
      ? `<div class="row"><span class="k">Model</span><span>${esc(current ?? "your default")}</span>${switcher}</div>`
      : "";
    const h = a.model_hint;
    if (!h || this.hintsDismissed.get(a.id) === h.model) return modelRow;
    const action = a.terminal
      ? `<button type="button" class="primary" data-switch-model="${esc(h.model)}" data-agent="${esc(a.id)}" title="${esc(why)}"${off}>Switch to ${esc(h.label)}</button>${busy ? `<span class="muted small">After this turn</span>` : ""}`
      : `<span class="muted small">Colony didn't start this session: run <code>/model ${esc(h.model)}</code> in it.</span>`;
    return `${modelRow}<div class="choice hint" role="group" aria-label="Model suggestion">
        <p><b>${esc(h.label)} may suit this better.</b> ${esc(h.reason)}</p>
        <div class="actions">${action}<button type="button" data-dismiss-hint="${esc(h.model)}" data-agent="${esc(a.id)}">Not now</button></div>
      </div>`;
  }

  private resumeHere(a: Agent): void {
    this.actions.newSession({ dir: a.project_dir ?? a.cwd ?? undefined, host: a.host, resume: a.session_id, resumeLabel: a.name });
  }

  /** Sessions with nothing going on: idle, ended, or crashed. Finished work
   *  waiting for review isn't included, so nothing unread gets cleared. */
  private idleSessions(): string[] {
    return [...this.daemon.agents.values()]
      .filter((a) => a.kind === "main" && (a.state === "idle" || a.state === "ended" || a.state === "crashed"))
      .map((a) => a.id);
  }

  private composeAgent(): Agent | undefined {
    return this.composeTarget ? this.daemon.agents.get(this.composeTarget) : undefined;
  }

  private sendReply(): void {
    const a = this.composeAgent();
    const text = this.reply.value.trim();
    if (!a?.terminal || !text) return;
    if (!this.daemon.sendText(a.terminal, text)) {
      this.toast("Not connected to colonyd; your message is still in the box.");
      return;
    }
    this.reply.value = "";
    this.drafts.delete(a.id);
  }

  /** The reply box lives outside the re-rendered panel so typing survives updates. */
  private renderCompose(a: Agent | undefined): void {
    const target = a && a.kind === "main" && a.terminal ? a.id : null;
    if (target !== this.composeTarget) {
      this.composeTarget = target;
      this.reply.value = (target && this.drafts.get(target)) ?? "";
    }
    this.compose.hidden = !target;
    // A permission prompt is showing in the session: offer Allow / Deny.
    const perm = document.getElementById("perm")!;
    const asking = !!a && !!target && a.state === "needs_input" && !a.permission && !a.reason?.startsWith("Waiting in its terminal");
    perm.hidden = !asking;
    if (asking) document.getElementById("perm-text")!.textContent = `Claude wants to run ${a!.reason ?? "a tool"}.`;
    if (a && target) {
      this.reply.placeholder =
        a.state === "awaiting_reply"
          ? "Answer the question…"
          : a.state === "working"
            ? "Message (Claude reads it after the current step)…"
            : "Message…";
    }
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
              `<b>${esc(a.name)}</b> ${esc(STATE_LABEL[a.state])} · ${since(a.state_since)}</button>` +
              (a.permission
                ? `<span class="quick"><button type="button" class="primary" data-decide="allow" data-req="${esc(a.permission.request_id)}" title="Allow ${esc(a.permission.tool)}">Allow</button><button type="button" class="danger" data-decide="deny" data-req="${esc(a.permission.request_id)}">Deny</button></span>`
                : ""),
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
    this.renderCompose(a);
    document.getElementById("inspector")!.classList.toggle("wide", needsWide(a));
    if (!a) {
      const idle = this.idleSessions().length;
      setHtml(this.panel, `<div class="k">Inspector</div><h2>Click a bot</h2>
        <p class="muted">Bots on the front porch need you: red ones are stuck, yellow ones want a permission or an answer.
        Bots holding a blue package at the review dock have finished a turn.</p>
        <p class="muted">Drag to pan, scroll to zoom, double-click a bot to zoom to its project, <kbd>0</kbd> to fit everything.</p>
        ${
          idle
            ? `<div class="actions"><button type="button" data-dismiss-idle="1" title="End idle, ended, and crashed sessions and clear them off the map">Dismiss ${idle} idle bot${idle === 1 ? "" : "s"}</button></div>`
            : ""
        }`);
      return;
    }
    const parent = a.parent_id ? this.daemon.agents.get(a.parent_id) : undefined;
    const kids = a.children.map((c) => this.daemon.agents.get(c)).filter((k): k is Agent => !!k);
    const sev = severity(a.state);
    const card = askCard(a, a.permission ? this.picks.get(a.permission.request_id) ?? [] : []);
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
      ${card || row(REASON_LABEL[a.state] ?? "Note", a.reason, "reason")}
      ${row("Session", a.title)}
      ${row("Objective", a.objective)}
      ${a.last_prompt !== a.objective ? row("Last prompt", a.last_prompt) : ""}
      ${toolRow}
      ${this.modelRows(a)}
      ${row("Where", `${hostLabel(a.host)} · ${originLabel(a)}${a.pid ? ` · pid ${a.pid}` : ""}`)}
      ${row("Folder", a.cwd, "mono")}
      ${row("Worktree", worktreeName(a.project_dir ?? a.cwd) && `${worktreeName(a.project_dir ?? a.cwd)} (branch colony/${worktreeName(a.project_dir ?? a.cwd)})`, "mono")}
      ${row("Tool calls", a.tool_calls ? String(a.tool_calls) : null)}
      ${
        kids.length
          ? `<div class="row"><span class="k">Subagents</span><div class="kids">${kids
              .map((k) => `<button type="button" class="kid" data-select="${esc(k.id)}"><span class="dot ${severity(k.state) ?? (k.state === "working" ? "ok" : "idle")}"></span>${esc(k.name)} · ${esc(STATE_LABEL[k.state])}</button>`)
              .join("")}</div></div>`
          : ""
      }
      ${row("Last message", a.last_message)}
      ${!a.hooks_seen && !a.terminal ? `<p class="muted small">Seen through the session registry only, so state is coarse. New sessions report full detail through hooks.</p>` : ""}
      <div class="actions">
        ${a.state === "ready_to_review" ? `<button type="button" data-ack="${esc(a.id)}">Mark reviewed</button>` : ""}
        ${a.state === "crashed" ? `<button type="button" data-ack="${esc(a.id)}">Clear</button>` : ""}
        ${a.kind === "main" && !a.terminal ? `<button type="button" class="primary" data-resume="${esc(a.id)}">Resume in Colony</button>` : ""}
        ${a.kind === "main" ? `<button type="button" data-new-here="${esc(a.id)}">New session here</button>` : ""}
        ${a.kind === "main" && !a.terminal ? `<button type="button" data-copy="${esc(resume)}">Copy resume command</button>` : ""}
        ${a.kind === "main" ? `<button type="button" class="danger" data-dismiss="${esc(a.id)}" title="End every copy of this session and clear it off the map. The conversation stays on disk and can still be resumed.">Dismiss</button>` : ""}
      </div>
      ${
        a.kind === "main" && a.terminal && resumeWarning(a)
          ? `<div class="choice">
              <p><b>Also open elsewhere.</b> ${esc(resumeWarning(a)!)}</p>
              <p class="muted small">Two copies write to one conversation. Keep the one in Colony and end the other.</p>
              <div class="actions"><button type="button" class="primary" data-end-other="${esc(a.id)}">End that copy</button></div>
            </div>`
          : ""
      }
      ${
        this.moveChoice === a.id && resumeWarning(a)
          ? `<div class="choice" role="group" aria-label="Resume options">
              <p><b>This session is still running.</b> ${esc(resumeWarning(a)!)}</p>
              <p class="muted small">Resuming here while it runs would put two copies on one conversation.</p>
              <div class="actions">
                <button type="button" class="primary" data-move="${esc(a.id)}">End it there, resume here</button>
                <button type="button" data-resume-anyway="${esc(a.id)}">Resume anyway</button>
                <button type="button" data-cancel-move="1">Cancel</button>
              </div>
            </div>`
          : ""
      }
      ${
        a.kind === "main" && !a.terminal
          ? `<p class="muted small">Colony didn't start this session, so it can't type into it. ${
              a.state === "ready_to_review" ? "Mark reviewed only moves it off the dock. " : ""
            }Resume it here to talk to it from the map, or reply in ${a.entrypoint === "claude-desktop" ? "the Claude desktop app" : "its terminal"}.</p>`
          : ""
      }`);
  }

  private async action(e: Event): Promise<void> {
    const el = (e.target as HTMLElement).closest<HTMLElement>("button");
    if (!el) return;
    if (el.dataset.select) this.actions.select(el.dataset.select);
    if (el.dataset.decide && el.dataset.req) this.decide(el, el.dataset.req, el.dataset.decide as PermissionChoice);
    if (el.dataset.pick) {
      const qi = Number(el.dataset.q);
      const p = this.pickFor(el.dataset.pick, qi);
      const label = el.dataset.label!;
      if (el.dataset.multi) p.labels = p.labels.includes(label) ? p.labels.filter((l) => l !== label) : [...p.labels, label];
      else p.labels = [label];
      p.other = "";
      this.schedule();
    }
    if (el.dataset.answer) {
      const a = this.selected ? this.daemon.agents.get(this.selected) : undefined;
      const answers = a && answersFrom(questionsOf(a), this.picks.get(el.dataset.answer) ?? []);
      if (!answers) return;
      if (!this.daemon.decide(el.dataset.answer, "allow", answers)) this.toast("Not connected to colonyd.");
      else el.closest(".actions")?.querySelectorAll("button").forEach((b) => b.setAttribute("disabled", ""));
    }
    if (el.dataset.terminalAnswer) {
      const a = this.selected ? this.daemon.agents.get(this.selected) : undefined;
      if (!this.daemon.decide(el.dataset.terminalAnswer, "pass")) this.toast("Not connected to colonyd.");
      else if (a) this.actions.showTerminal(a);
    }
    if (el.dataset.newHere) {
      const a = this.daemon.agents.get(el.dataset.newHere);
      this.actions.newSession({ dir: repoDir(a?.project_dir ?? a?.cwd) ?? undefined, host: a?.host });
    }
    if (el.dataset.resume) {
      const a = this.daemon.agents.get(el.dataset.resume);
      if (!a) return;
      if (resumeWarning(a)) {
        // Still running elsewhere: ask how to proceed, in the panel.
        this.moveChoice = a.id;
        this.schedule();
        return;
      }
      this.resumeHere(a);
    }
    if (el.dataset.switchModel) {
      const id = el.dataset.agent!;
      if (!this.daemon.setModel(id, el.dataset.switchModel)) this.toast("Not connected to colonyd.");
      else this.toast(`Restarting on ${modelLabel(el.dataset.switchModel)}; the conversation carries over…`);
    }
    if (el.dataset.dismissHint) {
      this.hintsDismissed.set(el.dataset.agent!, el.dataset.dismissHint);
      this.schedule();
    }
    if (el.dataset.endOther) {
      if (!this.daemon.terminate(el.dataset.endOther)) this.toast("Not connected to colonyd.");
      else el.textContent = "Ending…";
    }
    if (el.dataset.cancelMove) {
      this.moveChoice = null;
      this.schedule();
    }
    if (el.dataset.resumeAnyway) {
      const a = this.daemon.agents.get(el.dataset.resumeAnyway);
      this.moveChoice = null;
      if (a) this.resumeHere(a);
    }
    if (el.dataset.move) {
      const id = el.dataset.move;
      if (!this.daemon.terminate(id)) {
        this.toast("Not connected to colonyd.");
        return;
      }
      el.textContent = "Stopping the other copy…";
      el.setAttribute("disabled", "");
      // Resume once the daemon reports the old process gone.
      const started = Date.now();
      const wait = setInterval(() => {
        const a = this.daemon.agents.get(id);
        if (a && (a.state === "ended" || a.state === "crashed")) {
          clearInterval(wait);
          this.moveChoice = null;
          this.resumeHere(a);
        } else if (Date.now() - started > 8000) {
          clearInterval(wait);
          this.moveChoice = null;
          this.schedule();
          this.toast("The other copy didn't stop. Close it from the Claude desktop app's tray menu, then try again.");
        }
      }, 150);
    }
    if (el.dataset.dismiss) {
      const a = this.daemon.agents.get(el.dataset.dismiss);
      const busy = a && (a.state === "working" || a.state === "needs_input" || a.state === "awaiting_reply");
      if (!confirmed(el, busy ? "Still in use. Click again to end it" : "Click again to end and clear")) return;
      if (!this.daemon.dismiss(el.dataset.dismiss)) {
        this.toast("Not connected to colonyd.");
        return;
      }
      el.setAttribute("disabled", "");
      this.actions.select(null);
    }
    if (el.dataset.dismissIdle) {
      const ids = this.idleSessions();
      if (!confirmed(el, `Click again to end ${ids.length} idle session${ids.length === 1 ? "" : "s"}`)) return;
      if (!ids.every((id) => this.daemon.dismiss(id))) this.toast("Not connected to colonyd.");
      el.setAttribute("disabled", "");
    }
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

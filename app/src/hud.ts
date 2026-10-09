// DOM layer: state counts, the porch strip, the inspector panel, and the
// reply box for sessions Colony started.

import type { Daemon } from "./daemon";
import { costChart } from "./costchart";
import { latencyChart } from "./latencychart";
import { daySummary } from "./daysummary";
import { detail, playheadX, REPLAY_FRAMES, timelapse } from "./timelapse";
import { answersFrom, askCard, needsWide, questionsOf, type Pick } from "./ask";
import { md } from "./markdown";
import { repoDir, resumeWarning, worktreeName, type Prefill } from "./dialog";
import { compact, diffText, money, MODELS, modelLabel, ruleLabel, severity, spentToday, STATE_LABEL, stateLabel, whyText, WRONG_STATE_CHOICES, type Agent, type AgentState, type PermissionChoice } from "./types";

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
    // Scroll boxes (the activity feed, long prompts) keep their place across updates.
    const scrolls = new Map<string, number>();
    el.querySelectorAll<HTMLElement>("[data-scroll]").forEach((s) => scrolls.set(s.dataset.scroll!, s.scrollTop));
    // Keep the caret in an answer field the user is typing in.
    const f = document.activeElement as HTMLInputElement | null;
    const sel = f && el.contains(f) ? (f.dataset.other ? `[data-other="${f.dataset.other}"]` : f.dataset.draft ? `[data-draft="${f.dataset.draft}"]` : null) : null;
    const typing = sel && f ? { sel, at: f.selectionStart } : null;
    el.innerHTML = html;
    el.querySelectorAll<HTMLElement>("[data-scroll]").forEach((s) => (s.scrollTop = scrolls.get(s.dataset.scroll!) ?? 0));
    if (typing) {
      const n = el.querySelector<HTMLInputElement>(typing.sel);
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
  /** Open the sign-in or install terminal for a bot stuck on one. */
  fixNeed: (a: Agent) => Promise<void>;
  /** Resume a session in a Colony terminal right away, optionally sending a first message. */
  resumeNow: (a: Agent, prompt?: string) => Promise<void>;
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
  /** Replies typed for bots Colony didn't start, sent after resuming them. */
  private resumeDrafts = new Map<string, string>();
  /** Bots whose one-click resume is waiting on the other copy being dealt with, with the message to send. */
  private quick = new Map<string, string>();
  /** Answers chosen so far for held questions, by request id. */
  private picks = new Map<string, Pick[]>();
  private scheduled = false;
  /** Collapsible sections left open (they would otherwise close on every re-render). */
  private openSecs = new Set<string>();
  /** Time-lapse view: the frame shown, whether it is playing, and a request in flight. */
  private replayAt = 0;
  private replayTimer: number | null = null;
  private replayLoading = false;
  private replayShown: unknown = null;

  constructor(
    private daemon: Daemon,
    private actions: HudActions,
  ) {
    daemon.onChange(() => this.schedule());
    daemon.onError((m) => this.toast(m));
    daemon.onNotice((m) => this.toast(m));
    setInterval(() => this.tickDurations(), 1000);
    this.watchGating();
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
    // `toggle` doesn't bubble, so listen on the way down.
    this.panel.addEventListener(
      "toggle",
      (e) => {
        const d = e.target as HTMLDetailsElement;
        const key = d.dataset?.sec;
        if (!key) return;
        if (d.open) this.openSecs.add(key);
        else this.openSecs.delete(key);
      },
      true,
    );
    this.panel.addEventListener("input", (e) => {
      if ((e.target as HTMLElement).dataset.replayAt) {
        this.showFrame(Number((e.target as HTMLInputElement).value));
        return;
      }
      const el = e.target as HTMLInputElement;
      if (el.dataset.draft?.startsWith("reply:")) {
        this.resumeDrafts.set(el.dataset.draft.slice(6), el.value);
        return;
      }
      if (!el.dataset.other || !el.dataset.req) return;
      this.pickFor(el.dataset.req, Number(el.dataset.other)).other = el.value;
      // Only the send button's enabled state changes, so the field keeps focus.
      const btn = this.panel.querySelector<HTMLButtonElement>("[data-answer]");
      const a = this.selected ? this.daemon.agents.get(this.selected) : undefined;
      if (btn && a) btn.disabled = !answersFrom(questionsOf(a), this.picks.get(el.dataset.req) ?? []);
    });
    this.counts.addEventListener("click", (e) => {
      if (!(e.target as HTMLElement).closest("[data-show-leftovers], [data-show-rules], [data-show-cost]")) return;
      // The list lives in the inspector when no bot is selected.
      this.actions.select(null);
      this.schedule();
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
    // Menu keys and Allow/Deny press keys in the session, so menus can be
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
  /** Why the bot is in this state, with a one-click way to say it is wrong. */
  private whyRow(a: Agent): string {
    const choices = WRONG_STATE_CHOICES.filter(([s]) => s !== a.state)
      .map(([s, label]) => `<button type="button" data-wrong-for="${esc(a.id)}" data-wrong-is="${s}">${esc(label)}</button>`)
      .join("");
    return `<div class="fact why"><span class="k">Why</span><span>${esc(whyText(a))}
      <details class="menu" data-sec="wrong"${this.openSecs.has("wrong") ? " open" : ""}><summary title="Tell Colony what this bot should have shown. It is saved to ~/.colony/feedback.jsonl on this machine.">Wrong state?</summary><div class="menu-list">${choices}</div></details></span></div>`;
  }

  /** The opt-in Haiku classifier for unclear question-or-done endings. */
  private classifierRow(): string {
    const s = this.daemon.settings;
    return `<div class="fact"><span class="k">Unclear endings</span><span>
      <button type="button" data-classifier="${s.haiku_classifier ? "off" : "on"}" aria-pressed="${s.haiku_classifier}" title="Off by default. When on, only a bot's last message is sent to the Claude API (claude-haiku-5-5) to tell a question from a finished job, and only when the rules are unsure. Needs ANTHROPIC_API_KEY in the environment or ~/.colony/settings.json.">Haiku classifier: ${s.haiku_classifier ? "on" : "off"}</button>
      ${s.haiku_classifier && !s.haiku_key_present ? `<span class="muted small">No API key found: the rules decide.</span>` : ""}</span></div>`;
  }

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

  /** Resume right away in a Colony terminal, sending `text` first if there is any. */
  private async resumeQuick(a: Agent, text: string): Promise<void> {
    this.quick.delete(a.id);
    try {
      await this.actions.resumeNow(a, text || undefined);
      this.resumeDrafts.delete(a.id);
    } catch (err) {
      this.toast(err instanceof Error ? err.message : String(err));
    }
  }

  /** After the other copy is dealt with: finish the resume the user started. */
  private resumeFrom(a: Agent): void {
    if (this.quick.has(a.id)) void this.resumeQuick(a, this.quick.get(a.id) ?? "");
    else this.resumeHere(a);
  }

  private resumeHere(a: Agent): void {
    this.actions.newSession({ dir: a.project_dir ?? a.cwd ?? undefined, host: a.host, resume: a.session_id, resumeLabel: a.name });
  }

  /** Sessions with nothing going on: idle, ended, or crashed. Finished work
   *  waiting for review isn't included, so nothing unread gets cleared. */
  private idleSessions(): string[] {
    return [...this.daemon.agents.values()]
      .filter((a) => a.kind === "main" && !a.paused_at && (a.state === "idle" || a.state === "ended" || a.state === "crashed"))
      .map((a) => a.id);
  }

  private composeAgent(): Agent | undefined {
    return this.composeTarget ? this.daemon.agents.get(this.composeTarget) : undefined;
  }

  private sendReply(): void {
    const a = this.composeAgent();
    const text = this.reply.value.trim();
    if (!a?.terminal || !text) return;
    if (!this.daemon.reply(a.id, text)) {
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

  /** The tray's approvals kill switch (desktop app only); a stale one must not hide. */
  private gatingPaused = false;

  private watchGating(): void {
    const invoke = (window as unknown as { __TAURI__?: { core?: { invoke: <T>(cmd: string, args?: object) => Promise<T> } } }).__TAURI__?.core?.invoke;
    if (!invoke) return;
    const poll = () =>
      void invoke<boolean>("gating_paused")
        .then((p) => {
          if (p !== this.gatingPaused) {
            this.gatingPaused = p;
            this.schedule();
          }
        })
        .catch(() => {});
    poll();
    setInterval(poll, 5000);
    this.counts.addEventListener("click", (e) => {
      if (!(e.target as HTMLElement).closest("[data-resume-gating]")) return;
      void invoke<boolean>("gating_set", { paused: false }).then((p) => {
        this.gatingPaused = p;
        this.schedule();
      });
    });
  }

  private gatingChip(): string {
    return this.gatingPaused
      ? `<button type="button" class="chip warn" data-resume-gating title="Approvals gating is paused from the tray: permission requests are not held here and Claude Code's own prompt decides. Click to turn gating back on.">⏸ approvals paused</button>`
      : "";
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
      .join("") + this.gatingChip() + this.leftoverChip() + this.rulesChip() + this.spendChip());
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
              `<b>${esc(a.name)}</b> ${esc(stateLabel(a))} · ${since(a.state_since)}</button>` +
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

  /** Header chip: worktrees Colony left for the user to decide on. */
  /** Everything spent since local midnight, an estimate from list prices. */
  private spendChip(): string {
    const t = spentToday(this.daemon.spend, this.daemon.now());
    if (t.tokens === 0) return "";
    const note = t.partial ? " · partial" : "";
    const why = "Estimate from list prices, since local midnight, all projects." + (t.partial ? " Some sessions ran a model without a known price: their tokens are counted but not their cost." : "");
    return `<button type="button" class="chip spend" data-show-cost title="${esc(why)} Click for cost by project.">${money(t.usd)} today${note} · ${compact(t.tokens)} tokens</button>`;
  }

  /** Header chip: saved "allow always for project" rules. */
  private rulesChip(): string {
    const n = this.daemon.rules.length;
    return n
      ? `<button type="button" class="chip" data-show-rules title="Permissions you allowed always for a project; click to review or remove">⚖ <b>${n}</b> saved rule${n === 1 ? "" : "s"}</button>`
      : "";
  }

  /** The saved rules, each removable. Shown when no bot is selected. */
  private rulesList(): string {
    const rules = this.daemon.rules;
    const when = (t: number) => new Date(t).toLocaleDateString(undefined, { month: "short", day: "numeric" });
    const rows = rules
      .map(
        (r) => `<div class="rule row"><span class="k">${esc(r.project_name)}</span><span><b class="mono">${esc(ruleLabel(r))}</b> <span class="muted small">saved ${esc(when(r.created_at))}</span></span><button type="button" class="danger" data-del-rule="${esc(r.id)}" title="Remove this rule: ${esc(ruleLabel(r))} will ask again in ${esc(r.project_name)}">Remove</button></div>`,
      )
      .join("");
    return `<div class="k">Saved rules</div>${
      rules.length
        ? `<p class="muted small">Colony allows these without asking, only in the project shown and only when Claude Code suggests exactly this rule. Remove one and it asks again.</p>${rows}`
        : `<p class="muted small">None yet. Choose <b>Allow always for project</b> on a permission request to save one here.</p>`
    }`;
  }

  private leftoverChip(): string {
    const n = this.daemon.leftovers.length;
    return n
      ? `<button type="button" class="chip" data-show-leftovers title="Worktrees Colony kept because they hold work; click to review">⚠ <b>${n}</b> leftover worktree${n === 1 ? "" : "s"}</button>`
      : "";
  }

  /** Worktrees kept after a bot was dismissed, with what to do about each. */
  private leftoverList(): string {
    const items = this.daemon.leftovers;
    if (!items.length) return "";
    const name = (p: string) => p.replace(/[\\/]+$/, "").split(/[\\/]/).pop() ?? p;
    const rows = items
      .map((w) => {
        const id = esc(w.session_id);
        return `<div class="choice ask" role="group" aria-label="Leftover worktree">
          <p><b>${esc(name(w.repo))}</b> · <span class="mono">${esc(w.branch)}</span> · ${esc(hostLabel(w.host))}</p>
          <p class="muted">${esc(w.kept ?? "Kept")}${w.folder_gone ? " (the folder is gone; only the branch is left)" : ""}</p>
          ${w.folder_gone ? "" : `<pre class="ask-target">${esc(w.path)}</pre>`}
          <div class="actions">
            ${w.folder_gone ? "" : `<button type="button" class="primary" data-wt-open="${id}">Open folder</button>`}
            <button type="button" class="danger" data-wt-discard="${id}" title="Delete the folder and the branch, including anything uncommitted or unmerged">Discard</button>
            <button type="button" data-wt-forget="${id}" title="Stop listing this; the folder and branch stay as they are">Keep &amp; hide</button>
          </div>
        </div>`;
      })
      .join("");
    return `<div class="k">Leftover worktrees</div><p class="muted">These bots are done, but their worktrees hold work Colony won&#39;t delete on its own.</p>${rows}`;
  }

  private renderPanel(): void {
    // A new time-lapse arrived: start it from the beginning.
    // A request lost with the connection never answers.
    if (this.replayLoading && this.daemon.status !== "live") this.replayLoading = false;
    if (this.daemon.replay !== this.replayShown) {
      this.replayShown = this.daemon.replay;
      this.replayLoading = false;
      this.replayAt = 0;
      this.stopReplay();
    }
    const a = this.selected ? this.daemon.agents.get(this.selected) : undefined;
    this.renderCompose(a);
    document.getElementById("inspector")!.classList.toggle("wide", needsWide(a));
    if (!a) {
      const idle = this.idleSessions().length;
      setHtml(this.panel, `<div class="k">Inspector</div><h2>Click a bot</h2>
        <p class="muted">Bots on the front porch need you: red ones are stuck, yellow ones want a permission or an answer.
        Bots holding a blue package at the review dock have finished a turn.</p>
        <p class="muted">Drag to pan, scroll to zoom, double-click a bot to zoom to its project, <kbd>0</kbd> to fit everything.</p>
        ${daySummary(this.daemon.spend, this.daemon.latency, this.daemon.now())?.html ?? ""}
        ${timelapse(this.daemon.replay, this.replayAt, this.replayTimer !== null, this.replayLoading)}
        ${costChart(this.daemon.spend, this.daemon.now())}
        ${latencyChart(this.daemon.latency, this.daemon.now())}
        ${this.leftoverList()}
        ${this.rulesList()}
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
    const card = askCard(
      a,
      a.permission ? this.picks.get(a.permission.request_id) ?? [] : [],
      a.permission ? this.daemon.offers.get(a.permission.request_id) : undefined,
      this.resumeDrafts.get(a.id) ?? "",
      resumeWarning(a),
    );
    // Short facts sit in a two-column grid; long text gets a box that scrolls.
    const fact = (k: string, v: string | null | undefined, cls = "") =>
      v ? `<div class="fact"><span class="k">${k}</span><span class="${cls}">${esc(v)}</span></div>` : "";
    const field = (k: string, key: string, html: string, cls = "") =>
      html ? `<div class="field"><span class="k">${k}</span><div class="scrollbox ${cls}" data-scroll="${esc(a.id)}:${key}" tabindex="0">${html}</div></div>` : "";
    const text = (s: string | null | undefined) => (s ? esc(s) : "");

    const target = a.current_tool?.target?.replace(/\s+/g, " ");
    const nowRow = a.current_tool
      ? field("Now · " + since(a.current_tool.started_at), "now", `<b>${esc(a.current_tool.name)}</b>${target ? ` <span class="mono">${esc(target)}</span>` : ""}`, "short")
      : "";
    const usageRow = a.tokens && a.tokens.input + a.tokens.output + a.tokens.cache_read + a.tokens.cache_creation > 0
      ? `<div class="fact"><span class="k">Usage</span><span title="Estimate from list prices${a.kind === "main" ? "; includes subagents" : ""}${a.cost_partial ? ". Some tokens were from a model without a known price, so the cost is low." : ""}">${esc(`${compact(a.tokens.input + a.tokens.cache_creation + a.tokens.cache_read)} in · ${compact(a.tokens.output)} out · ${money(a.cost_usd ?? 0)}${a.cost_partial ? " partial" : ""}`)}</span></div>`
      : "";
    const collisionRow = a.collision
      ? `<div class="fact"><span class="k">Overlap</span><span class="reason" title="A warning only: nothing is blocked.">${esc(
          `${a.collision.scope === "file" ? "Also editing" : "Same folder as"} ${a.collision.with.map((w) => this.daemon.agents.get(w)?.name ?? "another bot").join(", ")}: ${a.collision.path.split(/[\\/]/).slice(-2).join("/")}`,
        )}</span></div>`
      : "";
    const diffRow = a.diff_stat
      ? `<div class="fact"><span class="k">Changes</span><span title="Uncommitted and untracked work in the session's folder, plus commits on its worktree branch">${esc(diffText(a.diff_stat))}</span></div>`
      : "";
    // Sessions Colony didn't start: its controls are limited, and the panel says so up front.
    const adopted = a.kind === "main" && !a.terminal && !a.paused_at;
    const live = a.state !== "ended" && a.state !== "crashed";
    const ours = !!a.terminal || !!a.paused_at || a.entrypoint === "colony";
    const originBadge = `<div class="origin ${ours ? "colony" : "adopted"}" title="${esc(ours ? "Colony started this session and can type into it." : "Colony found this session running. It can watch it, but not type into it.")}">${esc(ours ? "Started by Colony" : `Adopted · ${originLabel(a)}`)}</div>`;
    const resume = `claude --resume ${a.session_id}`;
    const wt = worktreeName(a.project_dir ?? a.cwd);
    const reply = a.last_message_full ?? a.last_message;
    // The card already shows a question in full; a finished turn's reason is its reply.
    const showReply = !!reply && !card;
    const showReason = !!a.reason && !card && !(a.state === "ready_to_review" && showReply);
    const secOpen = (k: string) => (this.openSecs.has(k) ? " open" : "");

    const main = a.kind === "main";
    const bar = [
      a.state === "ready_to_review" ? `<button type="button" class="primary" data-ack="${esc(a.id)}" title="Move it off the review dock">Mark reviewed</button>` : "",
      a.state === "crashed" ? `<button type="button" data-ack="${esc(a.id)}">Clear</button>` : "",
      a.terminal ? `<button type="button" data-open-term="${esc(a.id)}" title="Show this session's terminal (T)">Terminal</button>` : "",
      main && a.paused_at ? `<button type="button" class="primary" data-resume-paused="${esc(a.id)}" title="Start this session again with claude --resume; the conversation carries over">Resume</button>` : "",
      main && a.terminal && !a.paused_at
        ? a.pause_pending
          ? `<button type="button" data-cancel-pause="${esc(a.id)}">Cancel pause</button>`
          : `<button type="button" data-pause="${esc(a.id)}" title="${esc(a.state === "working" ? "Waits for this turn to end, then stops the session. Resume carries on with the conversation intact." : "Stops the session now (it is between turns). Resume carries on with the conversation intact.")}">Pause</button>`
        : "",
      adopted && live && a.host === "win" && a.pid ? `<button type="button" data-focus="${esc(a.id)}" title="Bring the terminal window this session runs in to the front (best effort)">Focus terminal</button>` : "",
      main && !a.terminal && !a.paused_at ? `<button type="button" class="primary" data-resume="${esc(a.id)}" title="Start this session in a Colony terminal so you can talk to it from here">Resume in Colony</button>` : "",
      main
        ? `<details class="menu" data-sec="more"${secOpen("more")}><summary>More</summary><div class="menu-list">
            <button type="button" data-new-here="${esc(a.id)}">New session here</button>
            ${!a.terminal ? `<button type="button" data-copy="${esc(resume)}">Copy resume command</button>` : ""}
            ${adopted && live && (a.pids.length || a.pid) ? `<button type="button" class="danger" data-kill="${esc(a.id)}" title="Ends this session's claude process (and its tools) and nothing else. The conversation stays on disk and can be resumed.">Kill process</button>` : ""}
            ${a.terminal ? `<button type="button" class="danger" data-end="${esc(a.id)}" title="Stop the session's process. It stays on the map as ended.">End session</button>` : ""}
            <button type="button" class="danger" data-dismiss="${esc(a.id)}" title="End every copy of this session and clear it off the map. The conversation stays on disk and can still be resumed.">Dismiss from map</button>
          </div></details>`
        : "",
    ].join("");

    setHtml(this.panel, `
      <div class="head">
        ${main ? originBadge : ""}
        <div class="k">${a.kind === "subagent" ? `Subagent of ${esc(parent?.name ?? "?")}` : esc(a.project_name ?? "unknown project")}</div>
        <h2>${esc(a.name)}</h2>
        <div><span class="pill ${sev ?? a.state}">${esc(stateLabel(a))}</span> <span class="muted">for ${since(a.state_since)}</span></div>
      </div>
      ${bar ? `<div class="actions bar">${bar}</div>` : ""}
      ${card}
      ${showReason ? field(REASON_LABEL[a.state] ?? "Note", "reason", text(a.reason), "short reason") : ""}
      ${main ? this.whyRow(a) : ""}
      ${
        a.auth_need
          ? `<div class="choice ask" role="group" aria-label="${a.auth_need.kind === "install" ? "Install needed" : "Sign-in needed"}">
              <p><b>${a.auth_need.kind === "install" ? `${esc(a.auth_need.label)} isn't installed.` : `Not signed in to ${esc(a.auth_need.label)}.`}</b> ${
                a.auth_need.kind === "install" ? "Install it here" : "Sign in here"
              } and this bot is told to retry.</p>
              <div class="actions"><button type="button" class="primary" data-fix-need="${esc(a.id)}">${
                a.auth_need.kind === "install" ? `Install ${esc(a.auth_need.label)}` : `Sign in to ${esc(a.auth_need.label)}`
              }</button></div>
            </div>`
          : ""
      }
      ${nowRow}
      ${field("Objective", "objective", text(a.objective_full ?? a.objective), "short")}
      ${showReply ? field(a.state === "ready_to_review" ? "Result" : "Latest reply", "reply", `<div class="md">${this.mdCached(reply!)}</div>`) : ""}
      <div class="facts">
        ${diffRow}${collisionRow}${usageRow}
      </div>
      ${this.modelRows(a)}
      ${
        kids.length
          ? `<div class="fact"><span class="k">Subagents</span><div class="kids">${kids
              .map((k) => `<button type="button" class="kid" data-select="${esc(k.id)}"><span class="dot ${severity(k.state) ?? (k.state === "working" ? "ok" : "idle")}"></span>${esc(k.name)} · ${esc(STATE_LABEL[k.state])}</button>`)
              .join("")}</div></div>`
          : ""
      }
      ${this.feed(a)}
      <details class="more" data-sec="details"${secOpen("details")}>
        <summary>Session details</summary>
        <div class="facts">
          ${a.title !== a.name ? fact("Session", a.title) : ""}
          ${fact("Where", `${hostLabel(a.host)} · ${originLabel(a)}${a.pid ? ` · pid ${a.pid}` : ""}`)}
          ${fact("Folder", a.cwd, "mono")}
          ${fact("Worktree", wt && `${wt} (branch colony/${wt})`, "mono")}
          ${fact("Tool calls", a.tool_calls ? String(a.tool_calls) : null)}
          ${fact("Session id", a.session_id, "mono")}
        </div>
        ${main ? this.classifierRow() : ""}
      </details>
      ${!a.hooks_seen && !a.terminal ? `<p class="muted small">Seen through the session registry only, so state is coarse. New sessions report full detail through hooks.</p>` : ""}
      ${
        main && a.terminal && resumeWarning(a)
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
        main && a.paused_at
          ? `<p class="muted small">Paused: Colony stopped this session's process between turns. Nothing is lost; the conversation is on disk. Resume starts it again with <code>--resume</code>.</p>`
          : a.pause_pending
            ? `<p class="muted small">Pausing as soon as this turn ends, so no work is cut off.</p>`
            : ""
      }
      ${
        main && !a.terminal && !a.paused_at
          ? `<p class="muted small"><b>Can't type here.</b> Colony didn't start this session, so it can only watch it (and Kill or Dismiss it). ${
              a.state === "ready_to_review" ? "Mark reviewed only moves it off the dock. " : ""
            }Use <b>Resume in Colony</b> to talk to it from the map, or reply in ${a.entrypoint === "claude-desktop" ? "the Claude desktop app" : "its terminal"}.</p>`
          : ""
      }`);
  }

  /** Rendered Markdown by source text: the panel re-renders on every event, and replies don't change. */
  private mdCache = new Map<string, string>();
  private mdCached(text: string): string {
    let html = this.mdCache.get(text);
    if (html === undefined) {
      html = md(text);
      // Oldest out first, so it stays a few dozen replies.
      if (this.mdCache.size >= 64) this.mdCache.delete(this.mdCache.keys().next().value!);
      this.mdCache.set(text, html);
    }
    return html;
  }

  /** What the bot has been doing, newest first, so the panel answers "what is it up to?" without the terminal. */
  private feed(a: Agent): string {
    const items = [...(a.activity ?? [])].reverse();
    if (!items.length) {
      return `<div class="field"><span class="k">Activity</span><p class="muted small">Nothing yet. Prompts, tool calls and replies appear here as they happen.</p></div>`;
    }
    const clock = (ms: number) => new Date(ms).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", second: "2-digit" });
    // Only the newest tool call can still be running.
    const live = a.current_tool ? items.findIndex((x) => x.kind === "tool") : -1;
    const rows = items
      .map((x, i) => {
        let mark: string;
        let body: string;
        switch (x.kind) {
          case "prompt":
            mark = "You";
            body = `<div class="act-text">${esc(x.text)}</div>`;
            break;
          case "reply":
            mark = "Claude";
            body = `<div class="act-text md">${this.mdCached(x.text)}</div>`;
            break;
          case "problem":
            mark = "!";
            body = `<div class="act-text mono">${esc(x.text)}</div>`;
            break;
          default:
            mark = i === live ? "…" : x.ok === false ? "✗" : x.ok ? "✓" : "·";
            body = `<div class="act-text mono">${esc(x.text)}</div>`;
        }
        const state = x.kind === "tool" ? (i === live ? "run" : x.ok === false ? "fail" : "") : "";
        return `<li class="act ${x.kind} ${state}"><time>${clock(x.at)}</time><span class="act-mark">${mark}</span>${body}</li>`;
      })
      .join("");
    return `<div class="field"><span class="k">Activity</span><ol class="feed" data-scroll="${esc(a.id)}:feed" tabindex="0">${rows}</ol></div>`;
  }

  /** Ask the daemon for a time-lapse ending now. */
  private loadReplay(spanMs: number): void {
    this.stopReplay();
    const to = this.daemon.now();
    if (!this.daemon.requestReplay(to - spanMs, to, REPLAY_FRAMES)) {
      this.toast("Not connected to colonyd.");
      return;
    }
    this.replayLoading = true;
    window.setTimeout(() => {
      if (!this.replayLoading) return;
      this.replayLoading = false;
      this.toast("The time-lapse did not load. Try again.");
      this.schedule();
    }, 15_000);
    this.schedule();
  }

  private toggleReplay(): void {
    const r = this.daemon.replay;
    if (!r) return;
    if (this.replayTimer !== null) {
      this.stopReplay();
    } else {
      if (this.replayAt >= r.frames.length - 1) this.replayAt = 0;
      this.replayTimer = window.setInterval(() => {
        if (this.replayAt >= r.frames.length - 1) this.stopReplay();
        else this.showFrame(this.replayAt + 1);
      }, 80);
    }
    this.schedule();
  }

  private stopReplay(): void {
    if (this.replayTimer !== null) window.clearInterval(this.replayTimer);
    this.replayTimer = null;
  }

  /** Move the playhead without re-rendering the panel, so dragging the scrubber isn't interrupted. */
  private showFrame(at: number): void {
    const r = this.daemon.replay;
    if (!r) return;
    this.replayAt = at;
    const x = playheadX(r, at).toFixed(2);
    this.panel.querySelector("#tl-head")?.setAttribute("x1", x);
    this.panel.querySelector("#tl-head")?.setAttribute("x2", x);
    const d = this.panel.querySelector("#tl-detail");
    if (d) d.innerHTML = detail(r, at);
    const s = this.panel.querySelector<HTMLInputElement>("#tl-scrub");
    if (s && Number(s.value) !== at) s.value = String(at);
  }

  private async action(e: Event): Promise<void> {
    const el = (e.target as HTMLElement).closest<HTMLElement>("button");
    if (!el) return;
    if (el.dataset.replay) this.loadReplay(Number(el.dataset.replay));
    if (el.dataset.replayPlay) this.toggleReplay();
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
    if (el.dataset.fixNeed) {
      const a = this.daemon.agents.get(el.dataset.fixNeed);
      if (!a) return;
      el.setAttribute("disabled", "");
      try {
        await this.actions.fixNeed(a);
      } catch (err) {
        this.toast(err instanceof Error ? err.message : String(err));
      } finally {
        el.removeAttribute("disabled");
      }
    }
    if (el.dataset.openTerm) {
      const a = this.daemon.agents.get(el.dataset.openTerm);
      if (a) this.actions.showTerminal(a);
    }
    if (el.dataset.end) {
      const a = this.daemon.agents.get(el.dataset.end);
      if (a?.terminal && confirmed(el, "Click again to end the session")) this.daemon.kill(a.terminal);
    }
    if (el.dataset.newHere) {
      this.openSecs.delete("more");
      this.schedule();
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
    if (el.dataset.pause) {
      if (!this.daemon.pause(el.dataset.pause)) this.toast("Not connected to colonyd.");
      else {
        const a = this.daemon.agents.get(el.dataset.pause);
        this.toast(a?.state === "working" ? "Pausing when this turn ends…" : "Pausing…");
      }
    }
    if (el.dataset.cancelPause && !this.daemon.cancelPause(el.dataset.cancelPause)) this.toast("Not connected to colonyd.");
    if (el.dataset.resumePaused) {
      if (!this.daemon.resume(el.dataset.resumePaused)) this.toast("Not connected to colonyd.");
      else {
        el.setAttribute("disabled", "");
        el.textContent = "Resuming…";
      }
    }
    if (el.dataset.resumeSend || el.dataset.resumeNow) {
      const a = this.daemon.agents.get((el.dataset.resumeSend ?? el.dataset.resumeNow)!);
      if (!a) return;
      const text = el.dataset.resumeSend ? (this.resumeDrafts.get(a.id) ?? "").trim() : "";
      if (el.dataset.resumeSend && !text) {
        this.toast("Type your answer first, or use Resume in Colony.");
        return;
      }
      if (resumeWarning(a)) {
        // Still running elsewhere: ask how to proceed, then carry on.
        this.quick.set(a.id, text);
        this.moveChoice = a.id;
        this.schedule();
        return;
      }
      void this.resumeQuick(a, text);
    }
    if (el.dataset.delRule && !this.daemon.deleteRule(el.dataset.delRule)) this.toast("Not connected to colonyd.");
    if (el.dataset.dismissHint) {
      this.hintsDismissed.set(el.dataset.agent!, el.dataset.dismissHint);
      this.schedule();
    }
    if (el.dataset.endOther) {
      if (!this.daemon.terminate(el.dataset.endOther)) this.toast("Not connected to colonyd.");
      else el.textContent = "Ending…";
    }
    if (el.dataset.focus && !this.daemon.focus(el.dataset.focus)) this.toast("Not connected to colonyd.");
    if (el.dataset.kill) {
      const a = this.daemon.agents.get(el.dataset.kill);
      const busy = a && (a.state === "working" || a.state === "needs_input" || a.state === "awaiting_reply");
      if (!confirmed(el, busy ? "Still in use. Click again to kill it" : "Click again to kill its process")) return;
      if (!this.daemon.terminate(el.dataset.kill)) this.toast("Not connected to colonyd.");
      else el.setAttribute("disabled", "");
    }
    if (el.dataset.cancelMove) {
      this.moveChoice = null;
      this.quick.clear();
      this.schedule();
    }
    if (el.dataset.resumeAnyway) {
      const a = this.daemon.agents.get(el.dataset.resumeAnyway);
      this.moveChoice = null;
      if (a) this.resumeFrom(a);
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
          this.resumeFrom(a);
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
    if (el.dataset.wtOpen && !this.daemon.openWorktree(el.dataset.wtOpen)) this.toast("Not connected to colonyd.");
    if (el.dataset.wtDiscard) {
      if (!confirmed(el, "Click again: deletes uncommitted work")) return;
      if (!this.daemon.discardWorktree(el.dataset.wtDiscard)) this.toast("Not connected to colonyd.");
      else el.setAttribute("disabled", "");
    }
    if (el.dataset.wtForget && !this.daemon.forgetWorktree(el.dataset.wtForget)) this.toast("Not connected to colonyd.");
    if (el.dataset.dismissIdle) {
      const ids = this.idleSessions();
      if (!confirmed(el, `Click again to end ${ids.length} idle session${ids.length === 1 ? "" : "s"}`)) return;
      if (!ids.every((id) => this.daemon.dismiss(id))) this.toast("Not connected to colonyd.");
      el.setAttribute("disabled", "");
    }
    if (el.dataset.ack && !this.daemon.ack(el.dataset.ack)) el.textContent = "Not connected; try again";
    if (el.dataset.wrongFor && el.dataset.wrongIs) {
      this.openSecs.delete("wrong");
      if (!this.daemon.stateFeedback(el.dataset.wrongFor, el.dataset.wrongIs)) this.toast("Not connected to colonyd.");
      this.schedule();
    }
    if (el.dataset.classifier) {
      if (!this.daemon.setClassifier(el.dataset.classifier === "on")) this.toast("Not connected to colonyd.");
    }
    if (el.dataset.copy) {
      const menu = el.closest(".menu") !== null;
      try {
        await navigator.clipboard.writeText(el.dataset.copy);
        if (menu) this.toast("Resume command copied.");
        else el.textContent = "Copied";
      } catch {
        if (menu) this.toast("Copy failed. The command is claude --resume " + this.selected);
        else el.textContent = "Copy failed; select the text below";
      }
      if (menu) {
        this.openSecs.delete("more");
        this.schedule();
      }
    }
  }
}

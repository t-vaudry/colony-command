// Connection to colonyd: snapshot + deltas over a WebSocket, reconnecting
// whenever the daemon restarts. Commands (ack, start a session, terminal
// input) go back over the same socket.

import { severity, type Agent, type Crew, type CrewCheck, type Found, type HostOption, type Leftover, type Offer, type PermissionChoice, type ProjectLatency, type ProjectSpend, type Replay, type Rule, type SpawnRequest } from "./types";
import type { NoticeAction } from "./budget";

interface DaemonInfo {
  port: number;
  token: string;
}

export interface DaemonSettings {
  /** Ask Claude Haiku about unclear question-or-done endings (off by default). */
  haiku_classifier: boolean;
  /** A key was found, so turning it on can work. The key itself is never sent. */
  haiku_key_present: boolean;
  /** Percent of the context window at which Colony warns that a bot's context is getting full. */
  context_pct?: number;
  /** Spend caps in USD; unset is no limit. */
  budget_bot_usd?: number | null;
  budget_day_usd?: number | null;
  budget_overrides?: Record<string, number>;
}

type Message =
  | { type: "snapshot"; now: number; agents: Agent[]; hosts?: HostOption[]; crews?: Crew[]; leftovers?: Leftover[]; found?: Found[]; spend?: Record<string, ProjectSpend>; latency?: Record<string, ProjectLatency>; rules?: Rule[]; offers?: Record<string, Offer>; settings?: DaemonSettings }
  | { type: "crews"; items: Crew[] }
  | { type: "crew_check"; req: number; error: string | null; warnings: string[] }
  | { type: "settings"; settings: DaemonSettings }
  | { type: "rules"; items: Rule[] }
  | ({ type: "permission_offer"; request_id: string } & Offer)
  | { type: "spend"; projects: Record<string, ProjectSpend> }
  | { type: "latency"; projects: Record<string, ProjectLatency> }
  | ({ type: "replay" } & Replay)
  | { type: "leftovers"; items: Leftover[] }
  | { type: "found"; items: Found[] }
  | { type: "batch"; msgs: Message[] }
  | { type: "upsert"; agent: Agent }
  | { type: "remove"; id: string }
  | { type: "spawned"; term: string; session_id: string }
  | { type: "term_data"; term: string; data: string; reset: boolean }
  | { type: "notice"; message: string; actions?: NoticeAction[] }
  | { type: "error"; message: string };

export type ConnectionStatus = "connecting" | "live" | "offline";

const RETRY_MS = 2000;
const OFFLINE_AFTER_MS = 6000;

async function readInfo(): Promise<DaemonInfo> {
  // Inside Tauri the shell reads ~/.colony/daemon.json for us.
  const tauri = (window as unknown as { __TAURI__?: { core?: { invoke: (cmd: string) => Promise<DaemonInfo> } } })
    .__TAURI__;
  if (tauri?.core?.invoke) return tauri.core.invoke("daemon_info");
  const r = await fetch("/__colony/daemon.json", { cache: "no-store" });
  if (!r.ok) throw new Error((await r.json().catch(() => null))?.error ?? `daemon info: HTTP ${r.status}`);
  return r.json();
}

function decode(b64: string): Uint8Array {
  const bin = atob(b64);
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out;
}

export class Daemon {
  agents = new Map<string, Agent>();
  hosts: HostOption[] = [];
  /** Built-in and saved crews: which models a new bot uses. */
  crews: Crew[] = [];
  /** Estimated spend per project key, in 15-minute buckets. */
  spend = new Map<string, ProjectSpend>();
  /** How long agents waited on a human, per project key, in 15-minute buckets. */
  latency = new Map<string, ProjectLatency>();
  /** The last time-lapse the daemon sent. */
  replay: Replay | null = null;
  /** Worktrees left behind for the user to decide on. */
  leftovers: Leftover[] = [];
  /** Sessions from before in Colony worktrees, offered to bring back. */
  found: Found[] = [];
  /** Saved "allow always for project" rules. */
  rules: Rule[] = [];
  /** What each held permission request would save as a project rule, by request id. */
  offers = new Map<string, Offer>();
  /** Daemon settings the map can change. */
  settings: DaemonSettings = { haiku_classifier: false, haiku_key_present: false };
  status: ConnectionStatus = "connecting";
  error: string | null = null;
  /** Daemon clock minus local clock, so durations match the daemon's timestamps. */
  skew = 0;
  private clockOffset: number | null = null;
  private clockTimer: ReturnType<typeof setInterval> | undefined;
  private info: DaemonInfo | null = null;
  private downSince: number | null = null;
  private ws: WebSocket | null = null;
  private listeners = new Set<() => void>();
  private termListeners = new Set<(term: string, bytes: Uint8Array, reset: boolean) => void>();
  private errorListeners = new Set<(message: string) => void>();
  private noticeListeners = new Set<(message: string, actions?: NoticeAction[]) => void>();
  /** Spawn replies arrive in the order spawns were sent. */
  private crewChecks = new Map<number, (c: CrewCheck) => void>();
  private crewCheckSeq = 0;
  private pendingSpawns: { resolve: (v: { term: string; session_id: string }) => void; reject: (e: Error) => void }[] = [];

  onChange(fn: () => void): void {
    this.listeners.add(fn);
  }

  onTermData(fn: (term: string, bytes: Uint8Array, reset: boolean) => void): void {
    this.termListeners.add(fn);
  }

  /** Something Colony did on its own that the user should hear about. */
  onNotice(fn: (message: string, actions?: NoticeAction[]) => void): void {
    this.noticeListeners.add(fn);
  }

  onError(fn: (message: string) => void): void {
    this.errorListeners.add(fn);
  }

  now(): number {
    return Date.now() + this.skew;
  }

  /** Inside a batch, listeners hear once at the end rather than per message. */
  private batching = false;

  private emit(): void {
    if (this.batching) return;
    for (const fn of this.listeners) fn();
  }

  start(): void {
    void this.connect();
  }

  private async connect(): Promise<void> {
    try {
      this.info = await readInfo();
    } catch (e) {
      this.setOffline(e instanceof Error ? e.message : String(e));
      return;
    }
    const ws = new WebSocket(`ws://127.0.0.1:${this.info.port}/ws?token=${this.info.token}`);
    ws.onmessage = (ev) => this.handle(JSON.parse(ev.data as string) as Message);
    ws.onclose = () => {
      this.ws = null;
      for (const p of this.pendingSpawns.splice(0)) p.reject(new Error("lost connection to colonyd"));
      this.setOffline("lost connection to colonyd");
    };
    this.ws = ws;
  }

  private setOffline(error: string): void {
    // A daemon restart takes a moment; only call it offline if it stays down.
    this.downSince ??= Date.now();
    this.status = Date.now() - this.downSince > OFFLINE_AFTER_MS ? "offline" : "connecting";
    this.error = error;
    this.emit();
    setTimeout(() => void this.connect(), RETRY_MS);
  }

  private handle(m: Message): void {
    switch (m.type) {
      case "snapshot":
        this.agents = new Map(m.agents.map((a) => [a.id, a]));
        this.hosts = m.hosts ?? this.hosts;
        this.crews = m.crews ?? this.crews;
        this.spend = new Map(Object.entries(m.spend ?? {}));
        this.latency = new Map(Object.entries(m.latency ?? {}));
        this.leftovers = m.leftovers ?? [];
        this.found = m.found ?? [];
        this.rules = m.rules ?? [];
        this.offers = new Map(Object.entries(m.offers ?? {}));
        this.settings = m.settings ?? this.settings;
        // The day cap counts from the map's local midnight, which only this clock knows.
        this.syncClock(true);
        this.skew = m.now - Date.now();
        this.status = "live";
        this.error = null;
        this.downSince = null;
        break;
      case "batch":
        this.batching = true;
        try {
          for (const sub of m.msgs) this.handle(sub);
        } finally {
          this.batching = false;
        }
        break;
      case "leftovers":
        this.leftovers = m.items;
        break;
      case "crews":
        this.crews = m.items;
        break;
      case "crew_check":
        this.crewChecks.get(m.req)?.({ error: m.error, warnings: m.warnings });
        this.crewChecks.delete(m.req);
        return;
      case "found":
        this.found = m.items;
        break;
      case "settings":
        this.settings = m.settings;
        break;
      case "rules":
        this.rules = m.items;
        break;
      case "permission_offer":
        this.offers.set(m.request_id, { rules: m.rules, project: m.project });
        // Requests are short-lived; keep the newest few hundred.
        for (const k of [...this.offers.keys()].slice(0, Math.max(0, this.offers.size - 300))) this.offers.delete(k);
        break;
      case "spend":
        for (const [k, p] of Object.entries(m.projects)) this.spend.set(k, p);
        break;
      case "replay":
        this.replay = { from: m.from, to: m.to, frames: m.frames };
        break;
      case "latency":
        for (const [k, p] of Object.entries(m.projects)) this.latency.set(k, p);
        break;
      case "upsert":
        this.agents.set(m.agent.id, m.agent);
        break;
      case "remove":
        this.agents.delete(m.id);
        break;
      case "spawned":
        this.pendingSpawns.shift()?.resolve({ term: m.term, session_id: m.session_id });
        return;
      case "term_data": {
        const bytes = decode(m.data);
        for (const fn of this.termListeners) fn(m.term, bytes, m.reset);
        return;
      }
      case "notice":
        for (const fn of this.noticeListeners) fn(m.message, m.actions);
        return;
      case "error": {
        // A failed spawn reports here too; otherwise it's about some other command.
        const pending = this.pendingSpawns.shift();
        if (pending) pending.reject(new Error(m.message));
        else for (const fn of this.errorListeners) fn(m.message);
        return;
      }
    }
    this.emit();
  }

  /** Commands go over the socket, so the browser's cross-origin rules don't apply. */
  private send(command: object): boolean {
    if (this.ws?.readyState !== WebSocket.OPEN) return false;
    this.ws.send(JSON.stringify(command));
    return true;
  }

  /** Main agents that need a human, most urgent first, then longest waiting. */
  porch(): Agent[] {
    const rank = { critical: 0, input: 1, review: 2 } as const;
    return [...this.agents.values()]
      .filter((a) => {
        const s = severity(a.state);
        return s === "critical" || s === "input";
      })
      .sort((a, b) => rank[severity(a.state)!] - rank[severity(b.state)!] || a.state_since - b.state_since);
  }

  /** "Wrong state": log what the bot showed and what the user says it should have been. */
  stateFeedback(id: string, shouldBe: string): boolean {
    return this.send({ type: "state_feedback", id, should_be: shouldBe });
  }

  /** Where bots' context counts as getting full, in percent of the window. */
  setContextPct(pct: number): boolean {
    return this.send({ type: "set_context_pct", pct });
  }

  /** Type /compact into a bot Colony started; the daemon refuses while it is working. Errors come back as toasts. */
  compact(id: string): boolean {
    return this.send({ type: "compact", id });
  }

  /** Turn the opt-in Haiku classifier for unclear endings on or off. */
  setClassifier(enabled: boolean): boolean {
    return this.send({ type: "set_classifier", enabled });
  }

  /** Tell the daemon this clock's UTC offset: on connecting (`force`), and again whenever it
   *  changes, e.g. when daylight saving starts in a long-running window. */
  private syncClock(force = false): void {
    const offset = -new Date().getTimezoneOffset();
    if (!force && offset === this.clockOffset) return;
    if (this.send({ type: "clock", utc_offset_min: offset })) this.clockOffset = offset;
    this.clockTimer ??= setInterval(() => this.syncClock(), 60_000);
  }

  /** Set the per-bot and per-day spend caps in USD; null switches one off. */
  setBudgets(bot: number | null, day: number | null): boolean {
    return this.send({ type: "set_budgets", bot, day });
  }

  /** A command a notice's button carries (raise a cap, undo a step down). */
  sendCommand(command: object): boolean {
    return this.send(command);
  }

  /** Mark finished work reviewed, or clear a crash. False if not connected. */
  ack(id: string): boolean {
    return this.send({ type: "ack", id });
  }

  /** Start (or resume) a session in a terminal Colony owns. */
  spawn(req: SpawnRequest): Promise<{ term: string; session_id: string }> {
    return new Promise((resolve, reject) => {
      if (!this.send({ type: "spawn", ...req })) {
        reject(new Error("not connected to colonyd"));
        return;
      }
      this.pendingSpawns.push({ resolve, reject });
    });
  }

  /** Whether this crew choice can start on `host`, and what will be limited. */
  checkCrew(choice: { host?: string; crew?: string; model?: string; advisor?: string; swarm?: string }): Promise<CrewCheck> {
    return new Promise((resolve) => {
      const req = ++this.crewCheckSeq;
      this.crewChecks.set(req, resolve);
      if (!this.send({ type: "check_crew", req, ...choice })) {
        this.crewChecks.delete(req);
        resolve({ error: null, warnings: [] });
      }
    });
  }

  /** Save a crew (new, or an edited copy of a built-in). Errors arrive as errors. */
  saveCrew(crew: Crew): boolean {
    return this.send({ type: "save_crew", crew });
  }

  /** Remove a saved crew, or reset a built-in one. */
  deleteCrew(id: string): boolean {
    return this.send({ type: "delete_crew", id });
  }

  /** Open the sign-in or install this bot is stuck on in a terminal; resolves with its id. */
  fixNeed(id: string, size: { cols: number; rows: number }): Promise<{ term: string; session_id: string }> {
    return new Promise((resolve, reject) => {
      if (!this.send({ type: "fix_need", id, ...size })) {
        reject(new Error("not connected to colonyd"));
        return;
      }
      this.pendingSpawns.push({ resolve, reject });
    });
  }

  attach(term: string): boolean {
    return this.send({ type: "attach", term });
  }

  detach(): boolean {
    return this.send({ type: "detach" });
  }

  input(term: string, data: string): boolean {
    return this.send({ type: "input", term, data });
  }

  /** Paste a message into the session and submit it. */
  sendText(term: string, text: string): boolean {
    return this.send({ type: "send", term, text });
  }

  /** Esc: stop the current action, keep the session. */
  interrupt(term: string): boolean {
    return this.send({ type: "interrupt", term });
  }

  resize(term: string, cols: number, rows: number): boolean {
    return this.send({ type: "resize", term, cols, rows });
  }

  kill(term: string): boolean {
    return this.send({ type: "kill", term });
  }

  /** Switch a Colony-started session's model (types /model in it). */
  setModel(id: string, model: string): boolean {
    return this.send({ type: "set_model", id, model });
  }

  /** Reply to a bot waiting on one; the daemon finds its terminal. */
  reply(id: string, text: string): boolean {
    return this.send({ type: "reply", id, text });
  }

  /** Stop a Colony-started session at its next safe point, between turns. */
  pause(id: string): boolean {
    return this.send({ type: "pause", id });
  }

  cancelPause(id: string): boolean {
    return this.send({ type: "cancel_pause", id });
  }

  /** Start a paused session again. */
  resume(id: string): boolean {
    return this.send({ type: "resume", id });
  }

  deleteRule(id: string): boolean {
    return this.send({ type: "delete_rule", id });
  }

  /** Answer a permission request Colony is holding. */
  decide(requestId: string, choice: PermissionChoice, answers?: Record<string, string>): boolean {
    return this.send({ type: "permission", request_id: requestId, choice, answers });
  }

  /** End a session Colony didn't start, so it can be resumed here. */
  terminate(id: string): boolean {
    return this.send({ type: "terminate", id });
  }

  /** Bring the window of a session Colony didn't start to the front. Errors come back as toasts. */
  focus(id: string): boolean {
    return this.send({ type: "focus", id });
  }

  /** Show a leftover worktree's folder in the file manager. */
  openWorktree(sessionId: string): boolean {
    return this.send({ type: "open_worktree", session_id: sessionId });
  }

  /** Delete a leftover worktree and its branch, uncommitted work included. */
  discardWorktree(sessionId: string): boolean {
    return this.send({ type: "discard_worktree", session_id: sessionId });
  }

  /** Stop listing a leftover; the folder and branch stay as they are. */
  forgetWorktree(sessionId: string): boolean {
    return this.send({ type: "forget_worktree", session_id: sessionId });
  }

  /** Bring these sessions from before back, paused; the rest of the offer isn't made again. */
  bringBack(pick: string[]): boolean {
    return this.send({ type: "bring_back", pick });
  }

  /** Done with a session: end every copy of it and clear it off the map. */
  dismiss(id: string): boolean {
    return this.send({ type: "dismiss", id });
  }

  /** Ask for a time-lapse of the logged events; the answer lands in `replay`. */
  requestReplay(from: number, to: number, frames: number): boolean {
    return this.send({ type: "replay", from, to, frames });
  }
}

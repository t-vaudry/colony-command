// Connection to colonyd: snapshot + deltas over a WebSocket, reconnecting
// whenever the daemon restarts. Commands (ack, start a session, terminal
// input) go back over the same socket.

import { severity, type Agent, type HostOption, type PermissionChoice, type SpawnRequest } from "./types";

interface DaemonInfo {
  port: number;
  token: string;
}

type Message =
  | { type: "snapshot"; now: number; agents: Agent[]; hosts?: HostOption[] }
  | { type: "upsert"; agent: Agent }
  | { type: "remove"; id: string }
  | { type: "spawned"; term: string; session_id: string }
  | { type: "term_data"; term: string; data: string; reset: boolean }
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
  status: ConnectionStatus = "connecting";
  error: string | null = null;
  /** Daemon clock minus local clock, so durations match the daemon's timestamps. */
  skew = 0;
  private info: DaemonInfo | null = null;
  private downSince: number | null = null;
  private ws: WebSocket | null = null;
  private listeners = new Set<() => void>();
  private termListeners = new Set<(term: string, bytes: Uint8Array, reset: boolean) => void>();
  private errorListeners = new Set<(message: string) => void>();
  /** Spawn replies arrive in the order spawns were sent. */
  private pendingSpawns: { resolve: (v: { term: string; session_id: string }) => void; reject: (e: Error) => void }[] = [];

  onChange(fn: () => void): void {
    this.listeners.add(fn);
  }

  onTermData(fn: (term: string, bytes: Uint8Array, reset: boolean) => void): void {
    this.termListeners.add(fn);
  }

  onError(fn: (message: string) => void): void {
    this.errorListeners.add(fn);
  }

  now(): number {
    return Date.now() + this.skew;
  }

  private emit(): void {
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
        this.skew = m.now - Date.now();
        this.status = "live";
        this.error = null;
        this.downSince = null;
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

  /** Answer a permission request Colony is holding. */
  decide(requestId: string, choice: PermissionChoice, answers?: Record<string, string>): boolean {
    return this.send({ type: "permission", request_id: requestId, choice, answers });
  }

  /** End a session Colony didn't start, so it can be resumed here. */
  terminate(id: string): boolean {
    return this.send({ type: "terminate", id });
  }

  /** Done with a session: end every copy of it and clear it off the map. */
  dismiss(id: string): boolean {
    return this.send({ type: "dismiss", id });
  }
}

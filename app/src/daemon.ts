// Connection to colonyd: snapshot + deltas over a WebSocket, reconnecting
// whenever the daemon restarts.

import { severity, type Agent } from "./types";

interface DaemonInfo {
  port: number;
  token: string;
}

type Message =
  | { type: "snapshot"; now: number; agents: Agent[] }
  | { type: "upsert"; agent: Agent }
  | { type: "remove"; id: string };

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

export class Daemon {
  agents = new Map<string, Agent>();
  status: ConnectionStatus = "connecting";
  error: string | null = null;
  /** Daemon clock minus local clock, so durations match the daemon's timestamps. */
  skew = 0;
  private info: DaemonInfo | null = null;
  private downSince: number | null = null;
  private ws: WebSocket | null = null;
  private listeners = new Set<() => void>();

  onChange(fn: () => void): void {
    this.listeners.add(fn);
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
      this.setOffline("lost connection to colonyd");
    };
    this.ws = ws;
  }

  /** Commands go over the socket, so the browser's cross-origin rules don't apply. */
  private send(command: object): boolean {
    if (this.ws?.readyState !== WebSocket.OPEN) return false;
    this.ws.send(JSON.stringify(command));
    return true;
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
    }
    this.emit();
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
}

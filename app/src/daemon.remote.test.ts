// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { Daemon } from "./daemon";
import { resetPlatform } from "./platform";
import { saveRemote } from "./remote";

class FakeSocket {
  static all: FakeSocket[] = [];
  static CONNECTING = 0;
  static OPEN = 1;
  readyState = 0;
  sent: string[] = [];
  onopen?: () => void;
  onmessage?: (e: { data: string }) => void;
  onclose?: () => void;
  constructor(public url: string) {
    FakeSocket.all.push(this);
  }
  send(m: string) {
    this.sent.push(m);
  }
  close() {
    this.readyState = 3;
    this.onclose?.();
  }
  open() {
    this.readyState = 1;
    this.onopen?.();
  }
  say(m: object) {
    this.onmessage?.({ data: JSON.stringify(m) });
  }
}

const snapshot = { type: "snapshot", now: Date.now(), agents: [] };

beforeEach(() => {
  vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout", "setInterval", "Date"] });
  FakeSocket.all = [];
  vi.stubGlobal("WebSocket", FakeSocket);
  (window as unknown as { __TAURI__: object }).__TAURI__ = { core: { invoke: async () => null } };
  Object.defineProperty(window.navigator, "userAgent", { value: "Mozilla/5.0 (Linux; Android 14)", configurable: true });
  resetPlatform();
  window.localStorage.clear();
  saveRemote({ host: "100.64.0.9", port: 7878, credential: "cred", name: "Tablet" });
});
afterEach(() => {
  vi.useRealTimers();
  vi.unstubAllGlobals();
  delete (window as unknown as { __TAURI__?: object }).__TAURI__;
  resetPlatform();
});

async function live(): Promise<{ d: Daemon; ws: FakeSocket }> {
  const d = new Daemon();
  d.start();
  await vi.advanceTimersByTimeAsync(0);
  const ws = FakeSocket.all[0];
  ws.open();
  ws.say(snapshot);
  return { d, ws };
}

describe("a paired device", () => {
  it("connects to the stored host and sends its credential as the first message, never in the URL", async () => {
    const { ws } = await live();
    expect(ws.url).toBe("ws://100.64.0.9:7878/ws");
    expect(JSON.parse(ws.sent[0])).toEqual({ type: "auth", credential: "cred" });
  });

  it("is live after the snapshot and learns its scope", async () => {
    const { d, ws } = await live();
    ws.say({ type: "you", scope: "view" });
    expect(d.status).toBe("live");
    expect(d.scope).toBe("view");
  });

  it("reconnects by itself after the link drops, backing off", async () => {
    const { d, ws } = await live();
    ws.close();
    expect(d.status).toBe("connecting");
    await vi.advanceTimersByTimeAsync(2100);
    expect(FakeSocket.all).toHaveLength(2);
    FakeSocket.all[1].close();
    await vi.advanceTimersByTimeAsync(2100);
    expect(FakeSocket.all).toHaveLength(2); // the second wait is longer than the first
    await vi.advanceTimersByTimeAsync(2000);
    expect(FakeSocket.all).toHaveLength(3);
  });

  it("shows offline once the PC stays unreachable", async () => {
    const { d, ws } = await live();
    ws.close();
    await vi.advanceTimersByTimeAsync(30000);
    expect(d.status).toBe("offline");
    expect(d.error).toMatch(/asleep|VPN/);
  });

  it("goes back to live when the network returns, without waiting out the backoff", async () => {
    const { d, ws } = await live();
    ws.close();
    await vi.advanceTimersByTimeAsync(30000);
    const before = FakeSocket.all.length;
    d.retryNow(); // what the browser's "online" event and the app coming back to the foreground call
    await vi.advanceTimersByTimeAsync(0);
    expect(FakeSocket.all.length).toBe(before + 1);
    const ws2 = FakeSocket.all[before];
    ws2.open();
    ws2.say(snapshot);
    expect(d.status).toBe("live");
  });

  it("listens for the network coming back and the app returning to the foreground", () => {
    const seen: string[] = [];
    const add = vi.spyOn(window, "addEventListener").mockImplementation(((t: string) => void seen.push(t)) as never);
    const doc = vi.spyOn(document, "addEventListener").mockImplementation(((t: string) => void seen.push(t)) as never);
    new Daemon().start();
    add.mockRestore();
    doc.mockRestore();
    expect(seen).toEqual(expect.arrayContaining(["online", "visibilitychange"]));
  });

  it("treats a socket that goes silent as dead and reconnects", async () => {
    const { d } = await live();
    await vi.advanceTimersByTimeAsync(50_000);
    expect(d.status).not.toBe("live");
    await vi.advanceTimersByTimeAsync(3000);
    expect(FakeSocket.all.length).toBeGreaterThan(1);
  });

  it("keeps a socket alive that hears heartbeats", async () => {
    const { d, ws } = await live();
    for (let i = 0; i < 6; i++) {
      await vi.advanceTimersByTimeAsync(15000);
      ws.say({ type: "hb" });
    }
    expect(d.status).toBe("live");
    expect(FakeSocket.all).toHaveLength(1);
  });

  it("is unpaired, with no retrying, when the PC says the credential is not valid", async () => {
    const { d, ws } = await live();
    ws.say({ type: "error", code: "unauthorized", message: "Missing or wrong credential." });
    expect(d.status).toBe("unpaired");
    expect(window.localStorage.getItem("colony.remote")).toBeNull();
    await vi.advanceTimersByTimeAsync(60000);
    expect(FakeSocket.all).toHaveLength(1);
  });

  it("says it is in the foreground, and keeps saying so, only when it has control", async () => {
    const { ws } = await live();
    ws.say({ type: "you", scope: "view" });
    await vi.advanceTimersByTimeAsync(20000);
    expect(ws.sent.some((m) => m.includes("presence"))).toBe(false);
    ws.say({ type: "you", scope: "control" });
    const presence = () => ws.sent.filter((m) => m.includes("presence")).map((m) => JSON.parse(m));
    expect(presence()).toEqual([{ type: "presence", foreground: true }]);
    await vi.advanceTimersByTimeAsync(31000);
    expect(presence().length).toBeGreaterThanOrEqual(3); // refreshed inside the daemon's 45 s expiry
  });

  it("answers a held request by sending the permission command", async () => {
    const { d, ws } = await live();
    ws.say({ type: "you", scope: "control" });
    expect(d.decide("req1", "allow")).toBe(true);
    expect(JSON.parse(ws.sent.at(-1)!)).toMatchObject({ type: "permission", request_id: "req1", choice: "allow" });
  });

  it("starts unpaired on a phone with nothing stored", async () => {
    window.localStorage.clear();
    const d = new Daemon();
    d.start();
    await vi.advanceTimersByTimeAsync(0);
    expect(d.status).toBe("unpaired");
    expect(FakeSocket.all).toHaveLength(0);
  });
});

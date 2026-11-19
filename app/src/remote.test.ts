import { describe, expect, it } from "vitest";
import { clearRemote, loadRemote, pair, parseAddress, parsePairingLink, saveRemote } from "./remote";
import { DEAD_AFTER_MS, isDead, retryDelay } from "./reconnect";

function mem() {
  const m = new Map<string, string>();
  return { getItem: (k: string) => m.get(k) ?? null, setItem: (k: string, v: string) => void m.set(k, v), removeItem: (k: string) => void m.delete(k) };
}

describe("stored remote", () => {
  it("round-trips and clears", () => {
    const s = mem();
    expect(loadRemote(s)).toBeNull();
    saveRemote({ host: "100.1.2.3", port: 7878, credential: "c", name: "Tablet" }, s);
    expect(loadRemote(s)?.credential).toBe("c");
    clearRemote(s);
    expect(loadRemote(s)).toBeNull();
  });
  it("treats damaged storage as unpaired", () => {
    const s = mem();
    s.setItem("colony.remote", "{nope");
    expect(loadRemote(s)).toBeNull();
    s.setItem("colony.remote", JSON.stringify({ host: 5 }));
    expect(loadRemote(s)).toBeNull();
  });
});

describe("addresses and pairing links", () => {
  it("parses host, host:port and bracketed v6", () => {
    expect(parseAddress("100.101.102.103")).toEqual({ host: "100.101.102.103", port: 7878 });
    expect(parseAddress(" pc.tail1234.ts.net:9000 ")).toEqual({ host: "pc.tail1234.ts.net", port: 9000 });
    expect(parseAddress("[fd7a:115c::1]:7878")).toEqual({ host: "[fd7a:115c::1]", port: 7878 });
  });
  it("rejects schemes, paths and bad ports", () => {
    for (const bad of ["", "http://x", "x/y", "x:0", "x:70000", "a b"]) expect(parseAddress(bad), bad).toBeNull();
  });
  it("reads a colony://pair link and ignores other QR codes", () => {
    expect(parsePairingLink("colony://pair?host=100.1.2.3&port=7878&code=1234-5678")).toEqual({ host: "100.1.2.3", port: 7878, code: "12345678" });
    expect(parsePairingLink("https://example.com/?code=12345678")).toBeNull();
    expect(parsePairingLink("colony://pair?host=x&code=12")).toBeNull();
    expect(parsePairingLink("not a url")).toBeNull();
  });
});

describe("pair", () => {
  const reply = (status: number, body: unknown = {}) => (async () => new Response(JSON.stringify(body), { status })) as unknown as typeof fetch;
  it("stores what the daemon returns and reports the scope", async () => {
    const r = await pair("100.1.2.3", 7878, "1234 5678", "Tablet", reply(200, { device_id: "d1", credential: "secret", scope: "view" }));
    expect(r).toMatchObject({ ok: true, scope: "view", remote: { host: "100.1.2.3", credential: "secret", deviceId: "d1" } });
  });
  it("says a wrong or expired code is wrong or expired", async () => {
    const r = await pair("h", 1, "12345678", "T", reply(403));
    expect(r.ok === false && r.message).toMatch(/wrong or has expired/);
  });
  it("says when pairing is locked", async () => {
    const r = await pair("h", 1, "12345678", "T", reply(429));
    expect(r.ok === false && r.message).toMatch(/Wait a minute/);
  });
  it("says when the PC can't be reached", async () => {
    const r = await pair("h", 1, "12345678", "T", (async () => { throw new TypeError("fetch failed"); }) as unknown as typeof fetch);
    expect(r.ok === false && r.message).toMatch(/Can't reach your PC/);
  });
  it("rejects a short code without asking the PC", async () => {
    let called = false;
    const r = await pair("h", 1, "123", "T", (async () => { called = true; return new Response(); }) as unknown as typeof fetch);
    expect(called).toBe(false);
    expect(r.ok).toBe(false);
  });
});

describe("reconnect timing", () => {
  it("backs off and caps", () => {
    expect(retryDelay(0)).toBe(2000);
    expect(retryDelay(1)).toBeGreaterThan(retryDelay(0));
    expect(retryDelay(50)).toBe(15000);
  });
  it("calls a quiet socket dead only after the limit", () => {
    expect(isDead(1000, 1000 + DEAD_AFTER_MS)).toBe(false);
    expect(isDead(1000, 1001 + DEAD_AFTER_MS)).toBe(true);
  });
});

// @vitest-environment jsdom
import { afterEach, describe, expect, it, vi } from "vitest";
import { detect, platform, resetPlatform } from "./platform";

const ua = (userAgent: string) => ({ navigator: { userAgent } });

describe("platform", () => {
  afterEach(() => {
    resetPlatform();
    delete (window as unknown as { __TAURI__?: unknown }).__TAURI__;
  });

  it("is a browser without a shell", () => {
    const p = detect({ open: vi.fn(), ...ua("Mozilla/5.0 (Windows NT 10.0)") });
    expect(p.kind).toBe("browser");
    expect(p.invoke).toBeNull();
    expect(p.has).toEqual({ updater: false, gating: false, setup: false, daemonControl: false });
  });

  it("is desktop in a desktop shell and mobile on a phone", () => {
    const tauri = { core: { invoke: vi.fn() } };
    expect(detect({ open: vi.fn(), __TAURI__: tauri, ...ua("Mozilla/5.0 (Windows NT 10.0)") }).kind).toBe("desktop");
    const m = detect({ open: vi.fn(), __TAURI__: tauri, ...ua("Mozilla/5.0 (Linux; Android 14)") });
    expect(m.kind).toBe("mobile");
    expect(m.has.updater).toBe(false);
  });

  it("forwards invoke and open_link to the shell", async () => {
    const invoke = vi.fn().mockResolvedValue(undefined);
    const p = detect({ open: vi.fn(), __TAURI__: { core: { invoke } }, ...ua("Windows") });
    await p.invoke!("daemon_restart");
    p.openLink("https://example.com");
    expect(invoke).toHaveBeenCalledWith("daemon_restart");
    expect(invoke).toHaveBeenCalledWith("open_link", { url: "https://example.com" });
  });

  it("opens links with window.open and has no version in a browser", async () => {
    const open = vi.fn();
    const p = detect({ open, ...ua("x") });
    p.openLink("https://example.com");
    expect(open).toHaveBeenCalledWith("https://example.com", "_blank", "noopener");
    expect(await p.appVersion()).toBeNull();
  });

  it("reads the app version from the shell", async () => {
    const p = detect({ open: vi.fn(), __TAURI__: { app: { getVersion: async () => "1.2.3" } }, ...ua("x") });
    expect(await p.appVersion()).toBe("1.2.3");
  });

  it("caches until reset", () => {
    const a = platform();
    expect(platform()).toBe(a);
    resetPlatform();
    expect(platform()).not.toBe(a);
  });
});

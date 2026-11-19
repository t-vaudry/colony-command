// @vitest-environment jsdom
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { RemotePanel } from "./remotepanel";

/** The real markup: the dialog and its top-bar button, copied out of index.html. */
function markup(): string {
  const html = readFileSync(resolve(process.cwd(), "index.html"), "utf8");
  const dlg = /<dialog id="remote-panel">[\s\S]*?<\/dialog>/.exec(html)![0];
  const btn = /<button[^>]*id="remote-btn"[^>]*>[\s\S]*?<\/button>/.exec(html)![0];
  return btn + dlg;
}

function fakeDaemon() {
  const listeners: (() => void)[] = [];
  const errors: ((m: string) => void)[] = [];
  return {
    remote: false,
    settings: {} as Record<string, unknown>,
    devices: [] as unknown[],
    pairing: null as null | { code: string; expires_at: number; remote: string | null },
    audit: null as null | unknown[],
    onChange: (f: () => void) => void listeners.push(f),
    onError: (f: (m: string) => void) => void errors.push(f),
    fire: () => listeners.forEach((f) => f()),
    fail: (m: string) => errors.forEach((f) => f(m)),
    listDevices: vi.fn(() => true),
    setRemoteListen: vi.fn(() => true),
    setNtfyUrl: vi.fn(() => true),
    setPreventSleep: vi.fn(() => true),
    createPairing: vi.fn(() => true),
    showAudit: vi.fn(() => true),
    hideAudit: vi.fn(),
    hidePairing: vi.fn(),
    setDeviceScope: vi.fn(() => true),
    revokeDevice: vi.fn(() => true),
    restart: vi.fn(async () => {}),
  };
}

const $ = (id: string) => document.getElementById(id)!;

beforeEach(() => {
  document.body.innerHTML = markup();
  HTMLDialogElement.prototype.showModal = function () {
    this.setAttribute("open", "");
  };
  HTMLDialogElement.prototype.close = function () {
    this.removeAttribute("open");
  };
});

function make(d = fakeDaemon()) {
  new RemotePanel(d as never);
  return d;
}

describe("remote panel", () => {
  it("is a top-bar button on the desktop and gone on a paired phone", () => {
    const d = make();
    expect(($("remote-btn") as HTMLButtonElement).hidden).toBe(false);
    d.remote = true;
    d.fire();
    expect(($("remote-btn") as HTMLButtonElement).hidden).toBe(true);
  });

  it("opens from the button, asks for the devices, and shows remote access as off", () => {
    const d = make();
    $("remote-btn").click();
    expect(($("remote-panel") as HTMLDialogElement).open).toBe(true);
    expect(d.listDevices).toHaveBeenCalled();
    expect($("rp-status").textContent).toBe("Off");
    expect($("rp-on").hidden).toBe(true);
  });

  it("saves the address typed, then offers a restart until the service listens there", () => {
    const d = make();
    $("remote-btn").click();
    const form = $("rp-addr-form") as HTMLFormElement;
    (form.elements.namedItem("addr") as HTMLInputElement).value = " 100.64.0.1 ";
    form.dispatchEvent(new Event("submit", { cancelable: true }));
    expect(d.setRemoteListen).toHaveBeenCalledWith(" 100.64.0.1 ");
    // The daemon echoes the saved setting, but it is not listening yet.
    d.settings = { remote_listen: "100.64.0.1" };
    d.fire();
    expect($("rp-restart").hidden).toBe(false);
    expect($("rp-restart-text").textContent).toContain("100.64.0.1");
    // After the restart it listens there, and the rest of the panel appears.
    d.settings = { remote_listen: "100.64.0.1", remote_listening: "100.64.0.1:7878" };
    d.fire();
    expect($("rp-restart").hidden).toBe(true);
    expect($("rp-status").textContent).toBe("On at 100.64.0.1:7878");
    expect($("rp-on").hidden).toBe(false);
  });

  it("shows the daemon's complaint about a bad address inside the dialog", () => {
    const d = make();
    $("remote-btn").click();
    d.fail("a wildcard address (0.0.0.0 or ::) would listen on every network");
    expect($("rp-error").textContent).toContain("wildcard");
  });

  it("pairs a device and shows the code", () => {
    const d = make();
    d.settings = { remote_listening: "100.64.0.1:7878" };
    $("remote-btn").click();
    $("rp-pair").click();
    expect(d.createPairing).toHaveBeenCalled();
    d.pairing = { code: "12345678", expires_at: Date.now() + 1000, remote: "100.64.0.1:7878" };
    d.fire();
    expect($("rp-code").hidden).toBe(false);
    expect($("rp-code").textContent).toContain("1234 5678");
  });

  it("lists devices and changes scope or revokes", () => {
    const d = make();
    d.settings = { remote_listening: "100.64.0.1:7878" };
    d.devices = [{ id: "d1", name: "Tablet", created_ms: 0, last_seen_ms: null, scope: "view" }];
    $("remote-btn").click();
    expect($("rp-devices").textContent).toContain("Tablet");
    ($("rp-devices").querySelector("[data-device-scope]") as HTMLElement).click();
    expect(d.setDeviceScope).toHaveBeenCalledWith("d1", "control");
    ($("rp-devices").querySelector("[data-revoke-device]") as HTMLElement).click();
    expect(d.revokeDevice).toHaveBeenCalledWith("d1");
  });

  it("toggles stay-awake and saves a phone-alert topic without keeping it on screen", () => {
    const d = make();
    d.settings = { remote_listening: "100.64.0.1:7878", prevent_sleep: false };
    $("remote-btn").click();
    $("rp-sleep").click();
    expect(d.setPreventSleep).toHaveBeenCalledWith(true);
    const form = $("rp-ntfy-form") as HTMLFormElement;
    const input = form.elements.namedItem("url") as HTMLInputElement;
    input.value = "https://ntfy.sh/secret-topic";
    form.dispatchEvent(new Event("submit", { cancelable: true }));
    expect(d.setNtfyUrl).toHaveBeenCalledWith("https://ntfy.sh/secret-topic");
    expect(input.value).toBe("");
  });

  it("doesn't overwrite an address while it is being typed", () => {
    const d = make();
    $("remote-btn").click();
    const input = (($("rp-addr-form") as HTMLFormElement).elements.namedItem("addr") as HTMLInputElement);
    input.focus();
    input.value = "100.64.";
    d.settings = { remote_listen: "100.1.1.1" };
    d.fire();
    expect(input.value).toBe("100.64.");
  });
});

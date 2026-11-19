// @vitest-environment jsdom
import { beforeEach, describe, expect, it, vi } from "vitest";
import { PairScreen, scopeText } from "./pairscreen";
import { loadRemote } from "./remote";

const HTML = readHtml();
function readHtml(): string {
  return `<dialog id="pair-screen"><form id="pair-form"><input name="address"><input name="code"><input name="name"><p id="pair-error"></p><button type="submit">Pair</button></form></dialog>`;
}

function fakeDaemon(status: string) {
  const listeners: (() => void)[] = [];
  return {
    status,
    error: null as string | null,
    onChange: (f: () => void) => void listeners.push(f),
    connectNow: vi.fn(),
    fire: () => listeners.forEach((f) => f()),
  };
}

const flush = () => new Promise((r) => setTimeout(r, 0));

beforeEach(() => {
  document.body.innerHTML = HTML;
  window.localStorage.clear();
  // jsdom has no <dialog> behaviour.
  HTMLDialogElement.prototype.showModal = function () {
    this.setAttribute("open", "");
  };
  HTMLDialogElement.prototype.close = function () {
    this.removeAttribute("open");
  };
});

function fill(address: string, code: string) {
  (document.querySelector("[name=address]") as HTMLInputElement).value = address;
  (document.querySelector("[name=code]") as HTMLInputElement).value = code;
  (document.querySelector("form") as HTMLFormElement).dispatchEvent(new Event("submit", { cancelable: true }));
}

describe("pairing screen", () => {
  it("opens while unpaired and closes once paired", () => {
    const d = fakeDaemon("unpaired");
    new PairScreen(d as never, () => {});
    expect(document.getElementById("pair-screen")!.hasAttribute("open")).toBe(true);
    d.status = "live";
    d.fire();
    expect(document.getElementById("pair-screen")!.hasAttribute("open")).toBe(false);
  });

  it("stores the credential and tells the caller the scope when the code is right", async () => {
    vi.stubGlobal("fetch", async () => new Response(JSON.stringify({ device_id: "d", credential: "secret", scope: "view" }), { status: 200 }));
    const d = fakeDaemon("unpaired");
    const paired = vi.fn();
    new PairScreen(d as never, paired);
    fill("100.64.0.9", "1234 5678");
    await flush();
    expect(paired).toHaveBeenCalledWith("view");
    expect(loadRemote()).toMatchObject({ host: "100.64.0.9", port: 7878, credential: "secret" });
    expect(d.connectNow).toHaveBeenCalled();
  });

  it("shows a clear message and stores nothing for a wrong or expired code", async () => {
    vi.stubGlobal("fetch", async () => new Response("no", { status: 403 }));
    const d = fakeDaemon("unpaired");
    new PairScreen(d as never, () => {});
    fill("100.64.0.9", "00000000");
    await flush();
    expect(document.getElementById("pair-error")!.textContent).toMatch(/wrong or has expired/);
    expect(loadRemote()).toBeNull();
    expect(document.getElementById("pair-screen")!.hasAttribute("open")).toBe(true);
  });

  it("fills the code from a pasted pairing link", () => {
    new PairScreen(fakeDaemon("unpaired") as never, () => {});
    const a = document.querySelector("[name=address]") as HTMLInputElement;
    a.value = "colony://pair?host=100.64.0.9&port=7878&code=87654321";
    a.dispatchEvent(new Event("input"));
    expect(a.value).toBe("100.64.0.9");
    expect((document.querySelector("[name=code]") as HTMLInputElement).value).toBe("87654321");
  });

  it("rejects an address that isn't one", () => {
    const d = fakeDaemon("unpaired");
    new PairScreen(d as never, () => {});
    fill("http://nope/x", "12345678");
    expect(document.getElementById("pair-error")!.textContent).toMatch(/VPN address/);
  });
});

describe("scopeText", () => {
  it("says what each scope allows", () => {
    expect(scopeText("view")).toBe("view only");
    expect(scopeText("control")).toMatch(/approvals/);
    expect(scopeText(null)).toBe("");
  });
});

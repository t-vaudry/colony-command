// @vitest-environment jsdom
import { beforeEach, describe, expect, it, vi } from "vitest";

const calls: string[] = [];
let onData: (d: string) => void = () => {};
let onTermData: (t: string, b: Uint8Array, reset: boolean, size?: { cols: number; rows: number }) => void = () => {};

vi.mock("@xterm/xterm", () => ({
  Terminal: class {
    cols = 80;
    rows = 24;
    options = {};
    loadAddon() {}
    onData(fn: (d: string) => void) {
      onData = fn;
    }
    resize(c: number, r: number) {
      calls.push(`resize:${c}x${r}`);
    }
    onResize() {}
    open() {}
    focus() {}
    refresh() {}
    write() {}
    reset() {
      calls.push("reset");
    }
  },
}));
vi.mock("@xterm/addon-fit", () => ({
  FitAddon: class {
    fit() {
      calls.push("fit");
    }
  },
}));
vi.mock("@xterm/addon-web-links", () => ({ WebLinksAddon: class {} }));
vi.mock("@xterm/xterm/css/xterm.css", () => ({}));

import { TerminalPane } from "./terminal";

function makePane() {
  const daemon = {
    onTermData: vi.fn((fn) => {
      onTermData = fn;
    }),
    remote: false,
    scope: null as "view" | "control" | null,
    interrupt: vi.fn(),
    attach: vi.fn((t: string) => calls.push(`attach:${t}`)),
    detach: vi.fn(() => calls.push("detach")),
    input: vi.fn(),
    resize: vi.fn(),
  };
  return { daemon, pane: new TerminalPane(daemon as never, vi.fn()) };
}

beforeEach(() => {
  calls.length = 0;
  document.body.innerHTML = `<div id="term-pane" hidden><span id="term-title"></span><button id="term-close"></button><div id="term-host"></div>
    <div id="term-keys" hidden><button data-key="esc">Esc</button><button data-key="up">Up</button><button id="term-ctrl">Ctrl</button><button data-key="ctrl-c">C</button><button id="term-interrupt">Interrupt</button></div></div>`;
  vi.stubGlobal("ResizeObserver", class { observe() {} });
  vi.stubGlobal("matchMedia", () => ({ matches: false, addEventListener() {} }));
});

describe("TerminalPane", () => {
  it("resets and fits before attaching, so the replay lands at the final size", () => {
    const { pane } = makePane();
    pane.show("t1", "one");
    expect(calls.indexOf("fit")).toBeGreaterThan(calls.indexOf("reset"));
    expect(calls.indexOf("fit")).toBeLessThan(calls.indexOf("attach:t1"));
  });

  it("re-attaches on every switch, not just the first", () => {
    const { daemon, pane } = makePane();
    pane.show("t1", "one");
    pane.show("t2", "two");
    pane.show("t2", "two");
    expect(daemon.attach.mock.calls.map((c) => c[0])).toEqual(["t1", "t2"]);
    expect(pane.attached).toBe("t2");
  });

  it("hide closes the pane and detaches", () => {
    const { daemon, pane } = makePane();
    pane.show("t1", "one");
    pane.hide();
    expect(pane.visible).toBe(false);
    expect(pane.attached).toBeNull();
    expect(daemon.detach).toHaveBeenCalledOnce();
  });

  const press = (sel: string) => document.querySelector<HTMLElement>(sel)!.click();

  describe("on a paired device", () => {
    it("draws at the session's size and never sends a resize", () => {
      const { daemon, pane } = makePane();
      daemon.remote = true;
      daemon.scope = "control";
      pane.show("t1", "one");
      onTermData("t1", new Uint8Array(), true, { cols: 132, rows: 40 });
      expect(calls).toContain("resize:132x40");
      expect(daemon.resize).not.toHaveBeenCalled();
      calls.length = 0;
      onTermData("t1", new Uint8Array(), false);
      expect(calls).not.toContain("fit");
    });

    it("shows the key bar, and the keys send the right bytes", () => {
      const { daemon, pane } = makePane();
      daemon.remote = true;
      daemon.scope = "control";
      pane.show("t1", "one");
      expect(document.getElementById("term-keys")!.hidden).toBe(false);
      press('[data-key="esc"]');
      press('[data-key="up"]');
      press('[data-key="ctrl-c"]');
      expect(daemon.input.mock.calls).toEqual([["t1", ""], ["t1", "[A"], ["t1", ""]]);
    });

    it("arms Ctrl for one typed character", () => {
      const { daemon, pane } = makePane();
      daemon.remote = true;
      daemon.scope = "control";
      pane.show("t1", "one");
      press("#term-ctrl");
      onData("d");
      onData("d");
      expect(daemon.input.mock.calls).toEqual([["t1", ""], ["t1", "d"]]);
    });

    it("interrupt sends the interrupt command", () => {
      const { daemon, pane } = makePane();
      daemon.remote = true;
      daemon.scope = "control";
      pane.show("t1", "one");
      press("#term-interrupt");
      expect(daemon.interrupt).toHaveBeenCalledWith("t1");
    });

    it("is read-only with a view scope: no key bar, no input", () => {
      const { daemon, pane } = makePane();
      daemon.remote = true;
      daemon.scope = "view";
      pane.show("t1", "one");
      expect(document.getElementById("term-keys")!.hidden).toBe(true);
      onData("x");
      expect(daemon.input).not.toHaveBeenCalled();
    });
  });

  it("on the desktop, the pane still owns the size", () => {
    const { daemon, pane } = makePane();
    pane.show("t1", "one");
    expect(daemon.resize).toHaveBeenCalled();
    expect(document.getElementById("term-keys")!.hidden).toBe(true);
  });
});

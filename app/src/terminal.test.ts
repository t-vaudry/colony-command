// @vitest-environment jsdom
import { beforeEach, describe, expect, it, vi } from "vitest";

const calls: string[] = [];

vi.mock("@xterm/xterm", () => ({
  Terminal: class {
    cols = 80;
    rows = 24;
    options = {};
    loadAddon() {}
    onData() {}
    onResize() {}
    open() {}
    focus() {}
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
vi.mock("@xterm/xterm/css/xterm.css", () => ({}));

import { TerminalPane } from "./terminal";

function makePane() {
  const daemon = {
    onTermData: vi.fn(),
    attach: vi.fn((t: string) => calls.push(`attach:${t}`)),
    detach: vi.fn(() => calls.push("detach")),
    input: vi.fn(),
    resize: vi.fn(),
  };
  return { daemon, pane: new TerminalPane(daemon as never, vi.fn()) };
}

beforeEach(() => {
  calls.length = 0;
  document.body.innerHTML = `<div id="term-pane" hidden><span id="term-title"></span><button id="term-close"></button><div id="term-host"></div></div>`;
  vi.stubGlobal("ResizeObserver", class { observe() {} });
  vi.stubGlobal("matchMedia", () => ({ addEventListener() {} }));
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
});

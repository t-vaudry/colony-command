// @vitest-environment jsdom
import { describe, expect, it, vi } from "vitest";
import { canCompact, ContextWatch, contextPercent, contextRow, contextSettingRow, contextText, contextWindow, overLimit, showContextNotices } from "./context";
import type { Agent } from "./types";

const bot = (extra: Partial<Agent> = {}): Agent =>
  ({ id: "s1", name: "Ada", kind: "main", state: "idle", terminal: "t1", model: "claude-sonnet-5-5", context_tokens: 142_000, ...extra }) as Agent;

describe("context window", () => {
  it("is 200k unless the model id says [1m] or the size does", () => {
    expect(contextWindow("claude-sonnet-5-5", 100_000)).toBe(200_000);
    expect(contextWindow(null, 0)).toBe(200_000);
    expect(contextWindow("claude-sonnet-5-5[1m]", 10)).toBe(1_000_000);
    expect(contextWindow("claude-opus-5-5", 250_000)).toBe(1_000_000);
  });

  it("rounds the percentage down", () => {
    expect(contextPercent(142_000, 200_000)).toBe(71);
    expect(contextPercent(139_999, 200_000)).toBe(69);
    expect(contextPercent(1, 0)).toBe(0);
  });
});

describe("meter", () => {
  it("reads 'Context 142k / 200k · 71%'", () => {
    expect(contextText(bot())).toBe("142k / 200k · 71%");
    expect(contextText(bot({ model: "claude-sonnet-5-5[1m]", context_tokens: 300_000 }))).toBe("300k / 1.0M · 30%");
    expect(contextText(bot({ context_tokens: 0 }))).toBeNull();
    expect(contextText(bot({ context_tokens: undefined }))).toBeNull();
  });

  it("warns at the limit and not before", () => {
    expect(overLimit(bot(), 70)).toBe(true);
    expect(overLimit(bot({ context_tokens: 139_000 }), 70)).toBe(false);
    expect(contextRow(bot(), 70)).toContain("ctx warn");
    expect(contextRow(bot({ context_tokens: 40_000 }), 70)).not.toContain("warn");
    expect(contextRow(bot({ context_tokens: 0 }), 70)).toBe("");
  });

  it("offers Compact only on a warned bot Colony started, enabled while idle", () => {
    expect(contextRow(bot(), 70)).toMatch(/data-compact="s1"(?! disabled)/);
    expect(contextRow(bot({ state: "working" }), 70)).toMatch(/data-compact="s1" disabled/);
    expect(contextRow(bot({ terminal: null }), 70)).not.toContain("data-compact");
    expect(contextRow(bot({ context_tokens: 40_000 }), 70)).not.toContain("data-compact");
    expect(canCompact(bot({ state: "needs_input" }))).toBe(false);
    expect(canCompact(bot({ state: "ready_to_review" }))).toBe(true);
  });

  it("marks the current limit in the setting row", () => {
    expect(contextSettingRow(80)).toContain('data-context-pct="80" aria-pressed="true"');
    expect(contextSettingRow(80)).toContain('data-context-pct="70" aria-pressed="false"');
  });
});

describe("notices", () => {
  it("fire once per crossing and again after the context shrinks", () => {
    const w = new ContextWatch();
    expect(w.check([bot({ context_tokens: 100_000 })], 70)).toEqual([]);
    expect(w.check([bot()], 70)).toEqual([{ text: "Ada's context is 71% full", compactId: "s1" }]);
    expect(w.check([bot({ context_tokens: 150_000 })], 70)).toEqual([]);
    expect(w.check([bot({ context_tokens: 20_000 })], 70)).toEqual([]);
    expect(w.check([bot()], 70)).toHaveLength(1);
  });

  it("ignores subagents, offers no Compact for bots Colony didn't start, survives a reconnect", () => {
    const w = new ContextWatch();
    expect(w.check([bot({ kind: "subagent" })], 70)).toEqual([]);
    expect(w.check([bot({ terminal: null })], 70)).toEqual([{ text: "Ada's context is 71% full", compactId: undefined }]);
    expect(w.check([], 70)).toEqual([]);
    expect(w.check([bot({ terminal: null })], 70)).toEqual([]);
  });

  it("shows a Compact button in the toast that sends the command", () => {
    document.body.innerHTML = '<div id="toast" hidden></div>';
    const toast = document.getElementById("toast")!;
    const compactBot = vi.fn();
    showContextNotices(toast, [{ text: "Ada's context is 71% full", compactId: "s1" }], compactBot);
    expect(toast.hidden).toBe(false);
    expect(toast.textContent).toContain("Ada's context is 71% full");
    toast.querySelector("button")!.click();
    expect(compactBot).toHaveBeenCalledWith("s1");
    showContextNotices(toast, [{ text: "A is 71% full" }], compactBot);
    expect(toast.querySelector("button")).toBeNull();
  });
});

import { describe, expect, it } from "vitest";
import { botBudgetRow, budgetClick, budgetSettings, budgetText, capText, dayNote, percent } from "./budget";

describe("budget text", () => {
  it("writes caps as exact amounts", () => {
    expect(capText(5)).toBe("$5");
    expect(capText(4.5)).toBe("$4.50");
  });

  it("rounds the share used", () => {
    expect(percent(3.1, 5)).toBe(62);
    expect(percent(6, 5)).toBe(120);
    expect(percent(1, 0)).toBe(0);
  });

  it("shows a bot's use of its cap, and nothing without one", () => {
    expect(budgetText(3.1, 5)).toBe("Budget ~$3.10 / $5 · 62%");
    expect(budgetText(3.1, null)).toBe("");
    expect(budgetText(3.1, undefined)).toBe("");
    expect(budgetText(3.1, 0)).toBe("");
  });

  it("puts the budget row in a main bot's panel only", () => {
    const row = botBudgetRow("main", 3.1, { budget_bot_usd: 5 });
    expect(row).toContain("Budget");
    expect(row).toContain("~$3.10 / $5 · 62%");
    expect(botBudgetRow("subagent", 3.1, { budget_bot_usd: 5 })).toBe("");
    expect(botBudgetRow("main", 3.1, {})).toBe("");
    expect(botBudgetRow("main", 6, { budget_bot_usd: 5 })).toContain("reason");
  });

  it("puts the day cap next to the day total", () => {
    expect(dayNote(12, { budget_day_usd: 20 })).toBe(" · budget $20 (60%)");
    expect(dayNote(12, { budget_day_usd: null })).toBe("");
    expect(dayNote(12, {})).toBe("");
  });
});

describe("budget settings", () => {
  it("offers Off and presets, marking the current cap", () => {
    const html = budgetSettings({ budget_bot_usd: 5 });
    expect(html).toContain('data-budget-bot="off" aria-pressed="false"');
    expect(html).toContain('data-budget-bot="5" aria-pressed="true"');
    expect(html).toContain('data-budget-day="off" aria-pressed="true"');
  });

  it("keeps a cap that is not a preset visible", () => {
    const html = budgetSettings({ budget_day_usd: 33 });
    expect(html).toContain('data-budget-day="33" aria-pressed="true"');
  });

  // The tests run without a DOM: only `dataset` is read.
  const button = (attr: string, value: string) => ({ dataset: { [attr]: value } }) as unknown as HTMLElement;

  it("sends the changed cap and keeps the other", () => {
    let sent: [number | null, number | null] | null = null;
    const set = (b: number | null, d: number | null) => ((sent = [b, d]), true);
    expect(budgetClick(button("budgetBot", "10"), { budget_bot_usd: 5, budget_day_usd: 25 }, set)).toBe(true);
    expect(sent).toEqual([10, 25]);
    expect(budgetClick(button("budgetDay", "off"), { budget_bot_usd: 5, budget_day_usd: 25 }, set)).toBe(true);
    expect(sent).toEqual([5, null]);
  });

  it("ignores other buttons, and reports a failed send", () => {
    expect(budgetClick(button("other", "1"), {}, () => true)).toBeNull();
    expect(budgetClick(button("budgetBot", "5"), {}, () => false)).toBe(false);
  });
});

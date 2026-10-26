import { money } from "./types";

const esc = (s: string) => s.replace(/[&<>"']/g, (c) => `&#${c.charCodeAt(0)};`);

/** The caps in the daemon's settings; unset or null is no limit. */
export interface BudgetCaps {
  budget_bot_usd?: number | null;
  budget_day_usd?: number | null;
  /** Caps raised for single bots, by agent id. */
  budget_overrides?: Record<string, number>;
}

/** A button on a notice. `command` goes back to the daemon as it is. */
export interface NoticeAction {
  label: string;
  command: object;
}

/** Amounts offered in the settings; a cap set another way shows up as its own button. */
export const BOT_PRESETS = [2, 5, 10, 20];
export const DAY_PRESETS = [10, 25, 50, 100];

/** A cap is an exact amount, not an estimate: "$5", "$4.50". */
export function capText(usd: number): string {
  return Number.isInteger(usd) ? `$${usd}` : `$${usd.toFixed(2)}`;
}

/** Share of the cap used, whole percent. */
export function percent(spent: number, cap: number): number {
  return cap > 0 ? Math.round((spent / cap) * 100) : 0;
}

const hasCap = (cap: number | null | undefined): cap is number => typeof cap === "number" && cap > 0;

/** "Budget ~$3.10 / $5 · 62%". Empty without a cap. */
export function budgetText(spent: number, cap: number | null | undefined): string {
  return hasCap(cap) ? `Budget ${money(spent)} / ${capText(cap)} · ${percent(spent, cap)}%` : "";
}

/** The bot panel's row; empty for a subagent or without a cap. */
export function botBudgetRow(kind: string, id: string, spent: number | undefined, caps: BudgetCaps): string {
  const own = caps.budget_overrides?.[id];
  const cap = own ?? caps.budget_bot_usd;
  if (kind !== "main" || !hasCap(cap)) return "";
  const used = percent(spent ?? 0, cap);
  const why = used >= 100 ? "Over its budget: Colony steps a bot it started down a notch after its turn." : "Subagents and the advisor count. At 80% you're warned; at 100% Colony drops the advisor, then the model a tier.";
  return `<div class="fact"><span class="k">Budget</span><span class="${used >= 100 ? "reason" : ""}" title="${esc(why)}">${esc(budgetText(spent ?? 0, cap).slice("Budget ".length))}${own === undefined ? "" : " (raised)"}</span></div>`;
}

/** What goes after "today" next to the day total: " · budget $20 (60%)". */
export function dayNote(spent: number, caps: BudgetCaps): string {
  const cap = caps.budget_day_usd;
  return hasCap(cap) ? ` · budget ${capText(cap)} (${percent(spent, cap)}%)` : "";
}

function choices(label: string, kind: "bot" | "day", presets: number[], current: number | null | undefined): string {
  const amounts = hasCap(current) && !presets.includes(current) ? [...presets, current].sort((a, b) => a - b) : presets;
  const button = (value: string, text: string, on: boolean) => `<button type="button" data-budget-${kind}="${value}" aria-pressed="${on}">${esc(text)}</button>`;
  return `<span class="k">${esc(label)}</span><span>${button("off", "Off", !hasCap(current))}${amounts.map((n) => button(String(n), capText(n), current === n)).join("")}</span>`;
}

/** The two caps, as rows of preset amounts. Both are off by default. */
export function budgetSettings(caps: BudgetCaps): string {
  const help =
    "Spend limits. At 80% you get a notice. At 100%, a bot Colony started drops its advisor after its turn, then its model a tier (Opus to Sonnet to Haiku); the swarm stays. Bots Colony didn't start only get the notice. Undo restores the models.";
  return (
    `<div class="fact" title="${esc(help)}">${choices("Budget per bot", "bot", BOT_PRESETS, caps.budget_bot_usd)}</div>` +
    `<div class="fact" title="${esc(help)}">${choices("Budget per day", "day", DAY_PRESETS, caps.budget_day_usd)}</div>`
  );
}

/** A click on one of the settings buttons: send the new caps. Returns false if it wasn't one,
 *  and the result of `set` otherwise. */
export function budgetClick(el: HTMLElement, caps: BudgetCaps, set: (bot: number | null, day: number | null) => boolean): boolean | null {
  const pick = (v: string | undefined, now: number | null | undefined) => (v === undefined ? (hasCap(now) ? now : null) : v === "off" ? null : Number(v));
  if (el.dataset.budgetBot === undefined && el.dataset.budgetDay === undefined) return null;
  return set(pick(el.dataset.budgetBot, caps.budget_bot_usd), pick(el.dataset.budgetDay, caps.budget_day_usd));
}

/** Put a notice's buttons in the toast. `send` returns false when not connected. */
export function toastActions(toast: HTMLElement, actions: NoticeAction[] | undefined, send: (command: object) => boolean, done: () => void): void {
  for (const a of actions ?? []) {
    const b = document.createElement("button");
    b.type = "button";
    b.textContent = a.label;
    b.addEventListener("click", () => {
      if (send(a.command)) done();
      else b.textContent = "Not connected";
    });
    toast.append(b);
  }
}

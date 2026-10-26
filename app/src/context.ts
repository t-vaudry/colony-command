// How full a bot's context is: the meter in its panel, the setting for when to warn, and the
// notice shown once each time a bot crosses the limit. The size comes from the daemon
// (`context_tokens`, the input side of the bot's newest response); the window logic mirrors
// colony-core's context.rs.

import { compact, type Agent } from "./types";

export const DEFAULT_WINDOW = 200_000;
export const LONG_WINDOW = 1_000_000;
/** Where the daemon starts warning unless settings say otherwise, in percent of the window. */
export const DEFAULT_PCT = 70;
/** Limits offered in the panel. */
export const PCT_CHOICES = [50, 60, 70, 80, 90];

const esc = (s: string) => s.replace(/[&<>"']/g, (c) => `&#${c.charCodeAt(0)};`);

/** 1M with the `[1m]` suffix on the model id; a size past 200k says the same (transcripts leave the suffix off). */
export function contextWindow(model: string | null | undefined, tokens: number): number {
  return /\[1m\]/i.test(model ?? "") || tokens > DEFAULT_WINDOW ? LONG_WINDOW : DEFAULT_WINDOW;
}

/** Whole percent of the window used, rounded down. */
export function contextPercent(tokens: number, window: number): number {
  return window > 0 ? Math.floor((tokens * 100) / window) : 0;
}

/** "142k / 200k · 71%", or null while the size is unknown. */
export function contextText(a: Pick<Agent, "context_tokens" | "model">): string | null {
  const t = a.context_tokens ?? 0;
  if (t <= 0) return null;
  const w = contextWindow(a.model, t);
  return `${compact(t)} / ${compact(w)} · ${contextPercent(t, w)}%`;
}

/** Whether the bot's context is at or past the limit (percent of its window). */
export function overLimit(a: Pick<Agent, "context_tokens" | "model">, limitPct: number): boolean {
  const t = a.context_tokens ?? 0;
  return t > 0 && contextPercent(t, contextWindow(a.model, t)) >= limitPct;
}

/** Colony can type `/compact` into a bot it started while the bot sits at its prompt. */
export function canCompact(a: Pick<Agent, "terminal" | "state" | "kind">): boolean {
  return a.kind === "main" && !!a.terminal && (a.state === "idle" || a.state === "ready_to_review");
}

/** The meter row of the bot panel. Empty until a size is known. */
export function contextRow(a: Agent, limitPct = DEFAULT_PCT): string {
  const text = contextText(a);
  if (!text) return "";
  const t = a.context_tokens ?? 0;
  const pct = Math.min(100, contextPercent(t, contextWindow(a.model, t)));
  const warn = overLimit(a, limitPct);
  const compactBtn =
    warn && a.kind === "main" && a.terminal
      ? ` <button type="button" data-compact="${esc(a.id)}"${canCompact(a) ? "" : " disabled"} title="${canCompact(a) ? "Types /compact into the bot's terminal." : "Available while the bot is idle."}">Compact</button>`
      : "";
  return `<div class="fact"><span class="k">Context</span><span class="ctx${warn ? " warn" : ""}" title="What every request re-sends: the input side of the latest response. Past ${limitPct}% of the window it gets expensive and Claude loses track of details sooner.">${esc(text)}<span class="ctx-bar" aria-hidden="true"><i style="width:${pct}%"></i></span>${compactBtn}</span></div>`;
}

/** Choices for where Colony warns, one button each. */
export function contextSettingRow(limitPct = DEFAULT_PCT): string {
  const buttons = PCT_CHOICES.map((p) => `<button type="button" data-context-pct="${p}" aria-pressed="${p === limitPct}">${p}%</button>`).join(" ");
  return `<div class="fact"><span class="k">Context warning</span><span title="Colony says so when a bot's context passes this share of its window, and tells crew sessions to lean on the swarm. Takes effect for sessions started from now on.">${buttons}</span></div>`;
}

export interface ContextNotice {
  /** The bot to compact, when Colony can type into it; otherwise absent. */
  compactId?: string;
  text: string;
}

/** Remembers which bots are over the limit, so each crossing is announced once. */
export class ContextWatch {
  private over = new Set<string>();

  /** Notices for main bots that have crossed the limit since the last call. A bot back under the limit (after a compact) can cross again. */
  check(agents: Iterable<Agent>, limitPct: number): ContextNotice[] {
    const out: ContextNotice[] = [];
    const seen = new Set<string>();
    for (const a of agents) {
      if (a.kind !== "main") continue;
      seen.add(a.id);
      if (!overLimit(a, limitPct)) {
        this.over.delete(a.id);
        continue;
      }
      if (this.over.has(a.id)) continue;
      this.over.add(a.id);
      const t = a.context_tokens ?? 0;
      out.push({ text: `${a.name}'s context is ${contextPercent(t, contextWindow(a.model, t))}% full`, compactId: a.terminal ? a.id : undefined });
    }
    // An empty list is a reconnect in progress, not every bot ending.
    if (seen.size) for (const id of this.over) if (!seen.has(id)) this.over.delete(id);
    return out;
  }
}

/** Shows notices in the toast; a single one with a Compact button when Colony can type into the bot. */
export function showContextNotices(toast: HTMLElement, notices: ContextNotice[], compactBot: (id: string) => void): void {
  if (!notices.length) return;
  toast.textContent = notices.map((n) => n.text).join(". ");
  if (notices.length === 1 && notices[0].compactId) {
    const id = notices[0].compactId;
    const b = document.createElement("button");
    b.type = "button";
    b.textContent = "Compact";
    b.title = "Types /compact into the bot's terminal, if it is idle.";
    b.addEventListener("click", () => {
      compactBot(id);
      toast.hidden = true;
    });
    toast.append(" ", b);
  }
  toast.hidden = false;
  clearTimeout(Number(toast.dataset.timer));
  toast.dataset.timer = String(setTimeout(() => (toast.hidden = true), 10000));
}

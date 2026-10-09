import { BUCKET_MS, compact, median, money, spentToday, waitText, type ProjectLatency, type ProjectSpend } from "./types";

const esc = (s: string) => s.replace(/[&<>"']/g, (c) => `&#${c.charCodeAt(0)};`);

interface Line {
  name: string;
  usd: number;
  partial: boolean;
  tokens: number;
  waits: number[];
}

/** Today's waits in a project, since local midnight. */
function waitsToday(p: ProjectLatency | undefined, now: number): number[] {
  const midnight = new Date(now);
  midnight.setHours(0, 0, 0, 0);
  const from = midnight.getTime() / BUCKET_MS;
  return Object.entries(p?.buckets ?? {}).flatMap(([b, w]) => (Number(b) >= from ? w : []));
}

/** What the day came to so far: spend and time-to-respond per project, since local
 *  midnight. Built only from the ledgers Colony already keeps, so it is as complete
 *  as they are (they reset when the daemon restarts). Null when nothing happened. */
export function daySummary(spend: Map<string, ProjectSpend>, latency: Map<string, ProjectLatency>, now: number): { html: string; text: string } | null {
  const lines: Line[] = [];
  for (const key of new Set([...spend.keys(), ...latency.keys()])) {
    const s = spentToday(spend, now, key);
    const waits = waitsToday(latency.get(key), now);
    if (s.tokens === 0 && !waits.length) continue;
    lines.push({ name: spend.get(key)?.name ?? latency.get(key)?.name ?? key, usd: s.usd, partial: s.partial, tokens: s.tokens, waits });
  }
  if (!lines.length) return null;
  lines.sort((a, b) => b.usd - a.usd || b.waits.length - a.waits.length);

  const usd = lines.reduce((n, l) => n + l.usd, 0);
  const partial = lines.some((l) => l.partial);
  const tokens = lines.reduce((n, l) => n + l.tokens, 0);
  const waits = lines.flatMap((l) => l.waits);
  const head = `${money(usd)}${partial ? " (partial)" : ""} · ${compact(tokens)} tokens · ${waits.length ? `${waits.length} answered, median ${waitText(median(waits))}` : "nothing waited on you"}`;
  const row = (l: Line) =>
    `${l.name}: ${money(l.usd)}${l.partial ? " (partial)" : ""}, ${l.waits.length ? `${l.waits.length} answered, median ${waitText(median(l.waits))}` : "nothing waited on you"}`;
  const day = new Date(now).toLocaleDateString(undefined, { weekday: "long", month: "short", day: "numeric" });
  const text = [`Colony, ${day}`, head, ...lines.map(row)].join("\n");
  const html =
    `<div class="k">Today so far</div><p><b>${esc(head)}</b></p>` +
    lines.map((l) => `<div class="row"><span class="k">${esc(l.name)}</span><span>${esc(row(l).slice(l.name.length + 2))}</span></div>`).join("") +
    `<p class="muted small">Since local midnight, from the cost and response ledgers; estimates at list prices.</p>` +
    `<div class="actions"><button type="button" data-copy="${esc(text)}">Copy summary</button></div>`;
  return { html, text };
}

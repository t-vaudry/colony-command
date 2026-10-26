import { modelLabel, type Agent, type CrewHealth } from "./types";

const esc = (s: string) => s.replace(/[&<>"']/g, (c) => `&#${c.charCodeAt(0)};`);

const times = (n: number) => `${n}×`;
const plural = (n: number, one: string, many = `${one}s`) => `${n} ${n === 1 ? one : many}`;

/** " · Sonnet builds · Haiku swarms · Opus reviews" for the Model row; empty when Colony didn't pick a crew. */
export function crewTag(a: Agent): string {
  return a.crew ? ` · ${esc(a.crew.label)}` : "";
}

/** "Delegated 7 searches · did 3 itself", or "" before the session has searched at all. */
export function delegationLine(h: CrewHealth | undefined): string {
  const delegated = (h?.delegated ?? []).reduce((n, d) => n + d.n, 0);
  const own = h?.self_searches ?? 0;
  if (!delegated && !own) return "";
  const by = (h?.delegated ?? []).map((d) => `${d.n} ${d.name.replace(/^swarm-/, "")}`).join(", ");
  return `Delegated ${plural(delegated, "search", "searches")}${by && delegated ? ` (${by})` : ""} · did ${own} itself`;
}

/** Share of the work handed to the swarm, 0..1; null with nothing counted. */
export function delegationShare(agents: Iterable<Agent>): number | null {
  let delegated = 0;
  let own = 0;
  for (const a of agents) {
    if (a.kind !== "main") continue;
    delegated += (a.crew_health?.delegated ?? []).reduce((n, d) => n + d.n, 0);
    own += a.crew_health?.self_searches ?? 0;
  }
  return delegated + own ? delegated / (delegated + own) : null;
}

/** Share of nudges followed by the advisor (or the audit), 0..1; null when none were sent. */
export function nudgeFollowShare(agents: Iterable<Agent>): number | null {
  let sent = 0;
  let followed = 0;
  for (const a of agents) {
    for (const s of a.crew_health?.nudges ?? []) {
      sent += s.sent;
      followed += s.followed;
    }
  }
  return sent ? followed / sent : null;
}

/** "Nudges: 3 sent, 2 followed (twice 1/1, done 1/2...)" or "". */
export function nudgeLine(h: CrewHealth | undefined): string {
  const stats = h?.nudges ?? [];
  const sent = stats.reduce((n, s) => n + s.sent, 0);
  if (!sent) return "";
  const followed = stats.reduce((n, s) => n + s.followed, 0);
  const kinds = stats.map((s) => `${esc(s.kind)} ${s.followed}/${s.sent}`).join(", ");
  return `Nudges: ${sent} sent, ${followed} followed (${kinds})`;
}

/** Swarm agents that stopped having used every turn they were allowed, one line per type. */
export function turnCapLines(kids: Agent[]): string[] {
  const by = new Map<string, { runs: number; hits: number; cap: number; calls: number }>();
  for (const k of kids) {
    if (!k.max_turns || !k.subagent_type) continue;
    const e = by.get(k.subagent_type) ?? { runs: 0, hits: 0, cap: k.max_turns, calls: 0 };
    e.runs += 1;
    e.calls += k.tool_calls;
    e.hits += k.hit_max_turns ? 1 : 0;
    by.set(k.subagent_type, e);
  }
  return [...by].map(([type, e]) => {
    const base = `${type}: ${plural(e.runs, "run")}, ${plural(e.calls, "tool call")}`;
    return e.hits ? `${base} · hit the ${e.cap}-turn cap ${times(e.hits)}${e.hits >= 2 ? ", the limit may be too low" : ""}` : base;
  });
}

/** "swarm-grep consulted Opus 2×": subagents are told never to consult the advisor. */
export function consultWarnings(a: Agent): string[] {
  const who = (modelLabel(a.advisor_model ?? null)?.split(" ")[0] ?? "the advisor");
  return (a.crew_health?.sub_consults ?? []).filter((c) => c.n > 0).map((c) => `${esc(c.name)} consulted ${esc(who)} ${times(c.n)}`);
}

/** The Crew block of the bot panel; empty for a bot with nothing to say. `kids` are its subagents. */
export function crewBlock(a: Agent, kids: Agent[]): string {
  const row = (k: string, v: string) => `<div class="row"><span class="k">${k}</span><span>${v}</span></div>`;
  if (a.kind === "subagent") {
    if (!a.max_turns) return "";
    const done = a.state === "ended";
    const turns = `${a.turns ?? 0} of ${a.max_turns} turns${a.hit_max_turns ? " · hit its cap" : ""}`;
    return `<div class="facts" data-crew>${row("Turns", `${turns}${done ? "" : " so far"}, ${plural(a.tool_calls, "tool call")}`)}</div>`;
  }
  const h = a.crew_health;
  const lines: string[] = [];
  const delegation = delegationLine(h);
  if (delegation) lines.push(row("Delegation", esc(delegation)));
  for (const l of turnCapLines(kids)) lines.push(row("Swarm", esc(l)));
  if (h && (h.main_consults || h.sub_consults?.length)) {
    lines.push(row("Advisor", `${plural(h.main_consults, "consult")} by the session`));
  }
  for (const w of consultWarnings(a)) lines.push(`<div class="row warn"><span class="k">Warning</span><span>${w}</span></div>`);
  const nudges = nudgeLine(h);
  if (nudges) lines.push(row("Nudges", nudges.replace(/^Nudges: /, "")));
  if (!lines.length) return "";
  return `<div class="facts" data-crew>${lines.join("")}</div>`;
}

/** One line for the day's summary, across the bots on the map; empty with nothing to say. */
export function crewRollup(agents: Iterable<Agent>): string {
  const list = [...agents];
  const share = delegationShare(list);
  const followed = nudgeFollowShare(list);
  const parts = [
    share === null ? "" : `${Math.round(share * 100)}% of searches delegated`,
    followed === null ? "" : `${Math.round(followed * 100)}% of nudges followed`,
  ].filter(Boolean);
  return parts.length ? `Crew: ${parts.join(" · ")}` : "";
}

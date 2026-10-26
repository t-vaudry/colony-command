import { describe, expect, it } from "vitest";
import { consultWarnings, crewBlock, crewRollup, crewTag, delegationLine, delegationShare, nudgeFollowShare, nudgeLine, turnCapLines } from "./crewhealth";
import type { Agent, CrewHealth } from "./types";

const health = (h: Partial<CrewHealth> = {}): CrewHealth => ({ delegated: [], self_searches: 0, main_consults: 0, sub_consults: [], nudges: [], ...h });
const agent = (extra: Partial<Agent> = {}): Agent => ({ id: "s", kind: "main", state: "working", tool_calls: 0, ...extra }) as Agent;
const swarm = (type: string, extra: Partial<Agent> = {}): Agent => agent({ kind: "subagent", subagent_type: type, state: "ended", ...extra });

describe("crew health", () => {
  it("names the crew in the model row", () => {
    expect(crewTag(agent({ crew: { id: "default", label: "Sonnet builds" } }))).toBe(" · Sonnet builds");
    expect(crewTag(agent())).toBe("");
    expect(crewTag(agent({ crew: { id: "x", label: "<b>" } }))).toBe(" · &#60;b&#62;");
  });

  it("says what was delegated and what it did itself", () => {
    const h = health({ delegated: [{ name: "swarm-search", n: 5 }, { name: "swarm-grep", n: 2 }], self_searches: 3 });
    expect(delegationLine(h)).toBe("Delegated 7 searches (5 search, 2 grep) · did 3 itself");
    expect(delegationLine(health({ self_searches: 2 }))).toBe("Delegated 0 searches · did 2 itself");
    expect(delegationLine(health())).toBe("");
    expect(delegationLine(undefined)).toBe("");
  });

  it("flags swarm agents that used every turn", () => {
    const kids = [
      swarm("swarm-grep", { max_turns: 10, hit_max_turns: true, tool_calls: 12 }),
      swarm("swarm-grep", { max_turns: 10, hit_max_turns: true, tool_calls: 11 }),
      swarm("swarm-search", { max_turns: 12, tool_calls: 4 }),
      swarm("Explore", { tool_calls: 99 }),
    ];
    expect(turnCapLines(kids)).toEqual([
      "swarm-grep: 2 runs, 23 tool calls · hit the 10-turn cap 2×, the limit may be too low",
      "swarm-search: 1 run, 4 tool calls",
    ]);
  });

  it("warns when a subagent consulted the advisor", () => {
    const a = agent({ advisor_model: "claude-opus-5-5", crew_health: health({ sub_consults: [{ name: "swarm-grep", n: 2 }] }) });
    expect(consultWarnings(a)).toEqual(["swarm-grep consulted Opus 2×"]);
    expect(consultWarnings(agent())).toEqual([]);
    expect(crewBlock(a, [])).toContain("swarm-grep consulted Opus 2×");
  });

  it("counts nudges followed", () => {
    const h = health({ nudges: [{ kind: "twice", sent: 1, followed: 1 }, { kind: "done", sent: 2, followed: 0 }] });
    expect(nudgeLine(h)).toBe("Nudges: 3 sent, 1 followed (twice 1/1, done 0/2)");
    expect(nudgeLine(health())).toBe("");
    expect(nudgeFollowShare([agent({ crew_health: h })])).toBeCloseTo(1 / 3);
    expect(nudgeFollowShare([agent()])).toBeNull();
  });

  it("shows a subagent its own turns", () => {
    const html = crewBlock(swarm("swarm-grep", { max_turns: 10, turns: 10, hit_max_turns: true, tool_calls: 9 }), []);
    expect(html).toContain("10 of 10 turns · hit its cap, 9 tool calls");
    expect(crewBlock(swarm("Explore"), [])).toBe("");
  });

  it("stays out of the way for a bot with nothing to report", () => {
    expect(crewBlock(agent(), [])).toBe("");
    expect(crewBlock(agent({ crew_health: health() }), [])).toBe("");
  });

  it("rolls up the day", () => {
    const a = agent({ crew_health: health({ delegated: [{ name: "swarm-search", n: 3 }], self_searches: 1, nudges: [{ kind: "plan", sent: 4, followed: 3 }] }) });
    const b = agent({ crew_health: health({ delegated: [{ name: "swarm-grep", n: 3 }], self_searches: 5 }) });
    expect(delegationShare([a, b])).toBeCloseTo(0.5);
    expect(crewRollup([a, b])).toBe("Crew: 50% of searches delegated · 75% of nudges followed");
    expect(crewRollup([agent()])).toBe("");
    expect(crewRollup([swarm("swarm-grep")])).toBe("");
  });
});

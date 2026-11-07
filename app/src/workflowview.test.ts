import { describe, expect, it } from "vitest";
import { followers, layout, loopRound, pathLine, phaseLine, runningByPhase, taken, workflowBlock } from "./workflowview";
import type { Agent, Workflow, WorkflowInfo } from "./types";

const wf: Workflow = {
  id: "feature",
  label: "Feature",
  description: "",
  phases: [
    { skill: "plan", when: "", next: ["build"] },
    { skill: "build", when: "", next: ["review", "plan"] },
    { skill: "review", when: "", next: [] },
    { skill: "docs", when: "", next: [] },
  ],
};
const visit = (skill: string, subagents = 0) => ({ skill, started_at: 1, ended_at: null, subagents });
const info = (visits: string[], active: string | null = visits.at(-1) ?? null): WorkflowInfo => ({ workflow: wf, visits: visits.map((s) => visit(s)), active });
const agent = (extra: Partial<Agent> = {}): Agent => ({ id: "s", kind: "main", state: "working", ...extra }) as Agent;

const bug: Workflow = {
  id: "bug",
  label: "Bug",
  description: "",
  phases: ["investigate", "triage", "develop", "test", "review", "publish"].map((skill) => ({ skill, when: "", next: [], instruction: skill === "triage" ? "Decide how bad it is." : "" })),
  loops: [{ from: "develop", to: "review", until: "the review finds nothing", max: 3 }],
};
const walk = (...skills: string[]): WorkflowInfo => ({ workflow: bug, visits: skills.map((skill) => visit(skill)), active: skills.at(-1) ?? null });

describe("workflow loops and custom phases", () => {
  it("leads each phase to the one listed after it unless told otherwise", () => {
    expect(followers(bug, 0)).toEqual(["triage"]);
    expect(followers(bug, 5)).toEqual([]);
    expect(followers(wf, 1)).toEqual(["review", "plan"]);
    expect(layout(bug).map((n) => n.col)).toEqual([0, 1, 2, 3, 4, 5]);
  });

  it("counts the rounds of a loop from the way the session moves through it", () => {
    const l = bug.loops![0];
    expect(loopRound(walk("investigate", "triage"), l)).toEqual({ round: 0, inside: false });
    expect(loopRound(walk("investigate", "triage", "develop"), l)).toEqual({ round: 1, inside: true });
    expect(loopRound(walk("triage", "develop", "test", "review"), l)).toEqual({ round: 1, inside: true });
    expect(loopRound(walk("triage", "develop", "test", "review", "develop", "test"), l)).toEqual({ round: 2, inside: true });
    expect(loopRound(walk("develop", "test", "review", "develop", "review", "develop", "publish"), l)).toEqual({ round: 3, inside: false });
    // Leaving and coming back starts again at round 1.
    expect(loopRound(walk("develop", "review", "triage", "develop"), l).round).toBe(1);
  });

  it("frames the loop, says which round it is in, and marks custom phases", () => {
    const html = workflowBlock(agent({ workflow: walk("investigate", "triage", "develop", "test", "review", "develop") }), []);
    expect(html).toContain('class="wf-loop active"');
    expect(html).toContain("↻ round 2 of 3 · until the review finds nothing");
    expect(html).toMatch(/wf-edge back taken loop-back/);
    expect(html).toContain("wf-node done custom");
    expect(html).toContain("triage (custom phase)");
    // Before the loop is reached it says how many rounds it may take.
    expect(workflowBlock(agent({ workflow: walk("investigate") }), [])).toContain("↻ up to 3 rounds · until");
  });

  it("makes room for nested loops, and draws no frame for a loop that names a missing phase", () => {
    const nested: Workflow = { ...bug, loops: [...bug.loops!, { from: "triage", to: "publish", until: "", max: 2 }, { from: "nope", to: "publish", until: "", max: 2 }] };
    const html = workflowBlock(agent({ workflow: { workflow: nested, visits: [], active: null } }), []);
    expect(html.match(/<g class="wf-loop/g)).toHaveLength(2);
    expect(html).not.toContain("nope");
  });

  it("escapes a loop's text", () => {
    const evil: Workflow = { ...bug, loops: [{ from: "develop", to: "test", until: "<img src=x>", max: 2 }] };
    expect(workflowBlock(agent({ workflow: { workflow: evil, visits: [], active: null } }), [])).not.toContain("<img");
  });
});

describe("workflow view", () => {
  it("puts phases in columns by distance from the first, unreached ones after", () => {
    const n = Object.fromEntries(layout(wf).map((x) => [x.skill, x.col]));
    expect(n).toEqual({ plan: 0, build: 1, review: 2, docs: 3 });
  });

  it("stacks phases that share a column", () => {
    const w: Workflow = { ...wf, phases: [{ skill: "a", when: "", next: ["b", "c"] }, { skill: "b", when: "", next: [] }, { skill: "c", when: "", next: [] }] };
    const [, b, c] = layout(w);
    expect(b.col).toBe(c.col);
    expect(c.y).toBeGreaterThan(b.y);
  });

  it("counts the transitions made and shows the path", () => {
    const i = info(["plan", "build", "plan", "build"]);
    expect(taken(i).get("plan>build")).toBe(2);
    expect(taken(i).get("build>plan")).toBe(1);
    expect(pathLine(i)).toBe("plan → build → plan → build");
  });

  it("counts subagents still working in each phase", () => {
    const kids = [agent({ kind: "subagent", phase: "build" }), agent({ kind: "subagent", phase: "build", state: "ended" }), agent({ kind: "subagent", phase: "plan" }), agent({ kind: "subagent" })];
    expect([...runningByPhase(kids)]).toEqual([["build", 1], ["plan", 1]]);
  });

  it("draws the active phase and the subagents in it", () => {
    const html = workflowBlock(agent({ workflow: info(["plan", "build"]) }), [agent({ kind: "subagent", phase: "build" })]);
    expect(html).toContain("Workflow · Feature");
    expect(html).toContain("In <b>build</b>");
    expect(html).toContain('class="wf-node active"');
    expect(html).toContain('class="wf-node done"');
    expect(html).toContain("1 subagent");
    expect(html).toContain("plan → build");
    expect(html).toContain("wf-edge taken");
  });

  it("says no phase has started yet, and stays out of the way otherwise", () => {
    expect(workflowBlock(agent({ workflow: info([], null) }), [])).toContain("No phase started yet");
    expect(workflowBlock(agent(), [])).toBe("");
    expect(workflowBlock(agent({ kind: "subagent", workflow: info(["plan"]) }), [])).toBe("");
  });

  it("escapes names from workflow files", () => {
    const w: Workflow = { id: "x", label: "<i>", description: "", phases: [{ skill: "a<b", when: "", next: [] }] };
    const html = workflowBlock(agent({ workflow: { workflow: w, visits: [], active: null } }), []);
    expect(html).not.toContain("<i>");
    expect(html).not.toContain("a<b");
  });

  it("draws a phase that leads to itself", () => {
    const w: Workflow = { id: "x", label: "X", description: "", phases: [{ skill: "a", when: "", next: ["a"] }] };
    expect(workflowBlock(agent({ workflow: { workflow: w, visits: [], active: null } }), [])).toContain("wf-edge back");
  });

  it("redraws a session identically, so the panel isn't rewritten for nothing, and keeps sessions' arrow ids apart", () => {
    const draw = (id: string) => workflowBlock(agent({ id, workflow: info(["plan"]) }), []);
    expect(draw("s1")).toBe(draw("s1"));
    const marker = (h: string) => /<marker id="([^"]+)"/.exec(h)![1];
    expect(marker(draw("s1"))).not.toBe(marker(draw("s2")));
    expect(marker(draw("a b<c"))).toMatch(/^[A-Za-z0-9_-]+$/);
  });

  it("draws at its natural size inside a scrolling box, so labels stay readable", () => {
    const html = workflowBlock(agent({ workflow: info(["plan"]) }), []);
    expect(html).toContain('class="wf-scroll"');
    expect(html).toMatch(/<svg class="wf-graph"[^>]* width="\d+" height="\d+"/);
  });

  it("names the phase a subagent started in", () => {
    expect(phaseLine(agent({ kind: "subagent", phase: "build" }))).toBe("Started in phase build");
    expect(phaseLine(agent({ kind: "subagent" }))).toBe("");
    expect(phaseLine(agent({ phase: "build" }))).toBe("");
  });
});

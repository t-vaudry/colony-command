import { describe, expect, it } from "vitest";
import { layout, pathLine, phaseLine, runningByPhase, taken, workflowBlock } from "./workflowview";
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

  it("names the phase a subagent started in", () => {
    expect(phaseLine(agent({ kind: "subagent", phase: "build" }))).toBe("Started in phase build");
    expect(phaseLine(agent({ kind: "subagent" }))).toBe("");
    expect(phaseLine(agent({ phase: "build" }))).toBe("");
  });
});

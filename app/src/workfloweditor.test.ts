import { describe, expect, it } from "vitest";
import { blank, clash, draftOf, loopProblem, movePhase, problem, removePhase, renameSkill, slug, toWorkflow, unusedSkills, type Draft, type DraftPhase } from "./workfloweditor";
import type { Workflow, WorkflowListing } from "./types";

const ph = (skill: string, next: string[] = [], extra: Partial<DraftPhase> = {}): DraftPhase => ({ skill, custom: false, instruction: "", when: "", next, ...extra });

const draft = (extra: Partial<Draft> = {}): Draft => ({
  id: "",
  label: "Feature",
  description: "",
  scope: "user",
  origin: null,
  phases: [ph("plan", ["build"]), ph("build", ["review", "plan"]), ph("review")],
  loops: [],
  ...extra,
});

/** investigate, triage (custom), [develop, test, review], publish */
const bug = (extra: Partial<Draft> = {}): Draft =>
  draft({
    label: "Bug",
    phases: [ph("investigate"), ph("triage", [], { custom: true, instruction: "Decide how bad it is." }), ph("develop"), ph("test"), ph("review"), ph("publish")],
    loops: [{ from: "develop", to: "review", until: "the review finds nothing", max: 3 }],
    ...extra,
  });

describe("workflow editor", () => {
  it("names the file after the workflow", () => {
    expect(slug("Feature work!")).toBe("feature-work");
    expect(slug("  --  ")).toBe("");
    expect(slug("A".repeat(80)).length).toBe(48);
  });

  it("says what stops a draft being saved", () => {
    expect(problem(draft())).toBeNull();
    expect(problem(draft({ label: " " }))).toMatch(/name/);
    expect(problem(draft({ label: "!!!" }))).toMatch(/letters or digits/);
    expect(problem(draft({ label: "!!!", id: "kept" }))).toBeNull();
    expect(problem(draft({ phases: [] }))).toMatch(/at least one phase/);
    expect(problem(draft({ phases: [ph(" ")] }))).toMatch(/Phase 1 has no skill/);
    expect(problem(draft({ phases: [ph("a b")] }))).toMatch(/isn't a skill name/);
    expect(problem(draft({ phases: [ph("a"), ph("a")] }))).toMatch(/twice/);
    expect(problem(blank("user"))).toMatch(/name/);
  });

  it("holds a custom phase to having a name and an instruction", () => {
    expect(problem(bug())).toBeNull();
    expect(problem(draft({ phases: [ph("", [], { custom: true })] }))).toMatch(/Phase 1 has no name/);
    expect(problem(draft({ phases: [ph("a b", [], { custom: true, instruction: "x" })] }))).toMatch(/isn't a name name|isn't a name/);
    expect(problem(draft({ phases: [ph("triage", [], { custom: true, instruction: "  " })] }))).toMatch(/no instruction/);
  });

  it("checks loops: both ends chosen, in order, in range, and not crossing", () => {
    const loops = (l: Draft["loops"]) => bug({ loops: l });
    const lp = (from: string, to: string, max = 3) => ({ from, to, until: "", max });
    expect(loopProblem(loops([lp("develop", "review"), lp("test", "test"), lp("triage", "publish")]))).toBeNull();
    expect(loopProblem(loops([lp("", "review")]))).toMatch(/Loop 1: choose/);
    expect(loopProblem(loops([lp("develop", "nope")]))).toMatch(/isn't in the list/);
    expect(loopProblem(loops([lp("review", "develop")]))).toMatch(/ends before it starts/);
    expect(loopProblem(loops([lp("develop", "test", 0)]))).toMatch(/between 1 and 20/);
    expect(loopProblem(loops([lp("develop", "test", 21)]))).toMatch(/between 1 and 20/);
    expect(loopProblem(loops([lp("develop", "test", 1.5)]))).toMatch(/between 1 and 20/);
    expect(loopProblem(loops([lp("develop", "test"), lp("test", "review")]))).toMatch(/crosses or repeats/);
    expect(loopProblem(loops([lp("develop", "test"), lp("develop", "test")]))).toMatch(/crosses or repeats/);
    expect(problem(loops([lp("review", "develop")]))).toMatch(/ends before it starts/);
  });

  it("builds the workflow, dropping what points at phases that are gone", () => {
    const d = draft({ label: " Feature work ", description: " Plan it. " });
    d.phases[0].next = ["build", "gone"];
    const w = toWorkflow(d);
    expect(w.id).toBe("feature-work");
    expect(w.label).toBe("Feature work");
    expect(w.description).toBe("Plan it.");
    expect(w.phases[0].next).toEqual(["build"]);
    expect(toWorkflow(draft({ id: "feature" })).id).toBe("feature");
  });

  it("builds the bug workflow with its custom phase and its loop", () => {
    const w = toWorkflow(bug());
    expect(w.phases.map((p) => [p.skill, p.instruction])).toEqual([["investigate", ""], ["triage", "Decide how bad it is."], ["develop", ""], ["test", ""], ["review", ""], ["publish", ""]]);
    expect(w.loops).toEqual([{ from: "develop", to: "review", until: "the review finds nothing", max: 3 }]);
    // Switching a phase back to a skill drops its instruction.
    const d = bug();
    d.phases[1].custom = false;
    expect(toWorkflow(d).phases[1].instruction).toBe("");
    // A loop that names a phase no longer there is left out.
    const e = bug();
    e.loops.push({ from: "gone", to: "review", until: "", max: 2 });
    expect(toWorkflow(e).loops).toHaveLength(1);
  });

  it("keeps links and loops when a phase is renamed", () => {
    const d = draft();
    renameSkill(d, 1, "make");
    expect(d.phases[0].next).toEqual(["make"]);
    expect(d.phases[1].skill).toBe("make");
    expect(d.phases[1].next).toEqual(["review", "plan"]);
    // A phase with no skill yet has nothing pointing at it.
    const e = draft({ phases: [ph(""), ph("x", [""])] });
    renameSkill(e, 0, "y");
    expect(e.phases[1].next).toEqual([""]);
    const b = bug();
    renameSkill(b, 2, "build");
    renameSkill(b, 4, "check");
    expect(b.loops[0]).toMatchObject({ from: "build", to: "check" });
  });

  it("leaves links alone while another phase has the same skill", () => {
    const d = draft({ phases: [ph("a", ["b"]), ph("b"), ph("b")] });
    renameSkill(d, 1, "c");
    expect(d.phases[0].next).toEqual(["b"]);
    const dup = draft({ phases: [ph("a", ["b"]), ph("b"), ph("b")] });
    removePhase(dup, 2);
    expect(dup.phases[0].next).toEqual(["b"]);
    const e = draft({ phases: [ph("a", ["b"]), ph("b ")] });
    renameSkill(e, 1, "c ");
    expect(e.phases[0].next).toEqual(["c"]);
  });

  it("warns before a new workflow replaces one, or one saved for the user is hidden by the project's", () => {
    const l: WorkflowListing = { workflows: [{ id: "feature", label: "Feature", description: "", phases: [] }], from_project: ["feature"], problems: [], skills: [] };
    expect(clash(draft({ label: "Feature" }), l)).toMatch(/already a workflow called "Feature"/);
    expect(clash(draft({ label: "Other" }), l)).toBeNull();
    expect(clash(draft({ id: "feature", label: "Feature", scope: "project" }), l)).toBeNull();
    expect(clash(draft({ id: "feature", label: "Feature", scope: "user" }), l)).toMatch(/project has its own/);
  });

  it("removes a phase, every way to it, and the loops that ran to or from it", () => {
    const d = draft();
    removePhase(d, 1);
    expect(d.phases.map((p) => p.skill)).toEqual(["plan", "review"]);
    expect(d.phases[0].next).toEqual([]);
    const b = bug();
    removePhase(b, 3);
    expect(b.loops).toHaveLength(1);
    removePhase(b, 3);
    expect(b.loops).toEqual([]);
  });

  it("moves phases, staying within the list", () => {
    const d = draft();
    movePhase(d, 0, -1);
    expect(d.phases.map((p) => p.skill)).toEqual(["plan", "build", "review"]);
    movePhase(d, 0, 1);
    expect(d.phases.map((p) => p.skill)).toEqual(["build", "plan", "review"]);
    movePhase(d, 2, 1);
    expect(d.phases.map((p) => p.skill)).toEqual(["build", "plan", "review"]);
  });

  it("turns an existing workflow into a draft that doesn't share its arrays", () => {
    const w: Workflow = {
      id: "f",
      label: "F",
      description: "d",
      phases: [{ skill: "a", when: "w", next: ["a"] }, { skill: "t", instruction: "Think.", when: "", next: [] }],
      loops: [{ from: "a", to: "t", until: "u", max: 2 }],
    };
    const d = draftOf(w, true);
    expect(d.scope).toBe("project");
    expect(d.origin).toBe("project");
    expect(draftOf(w, false).scope).toBe("user");
    expect(d.phases.map((p) => [p.custom, p.instruction])).toEqual([[false, ""], [true, "Think."]]);
    expect(d.loops).toEqual(w.loops);
    d.phases[0].next.push("z");
    d.loops[0].max = 9;
    expect(w.phases[0].next).toEqual(["a"]);
    expect(w.loops![0].max).toBe(2);
    // An older workflow has no loops at all.
    expect(draftOf({ ...w, loops: undefined }, true).loops).toEqual([]);
  });

  it("offers installed skills no skill phase uses yet", () => {
    const skills = [
      { name: "plan", description: "", source: "project" as const },
      { name: "ship", description: "", source: "user" as const },
    ];
    expect(unusedSkills(draft(), skills).map((s) => s.name)).toEqual(["ship"]);
    // A custom phase named like a skill doesn't use it up.
    expect(unusedSkills(draft({ phases: [ph("ship", [], { custom: true, instruction: "x" })] }), skills).map((s) => s.name)).toEqual(["plan", "ship"]);
  });
});

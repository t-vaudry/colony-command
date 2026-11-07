import { describe, expect, it } from "vitest";
import { blank, clash, draftOf, movePhase, problem, removePhase, renameSkill, slug, toWorkflow, unusedSkills, type Draft } from "./workfloweditor";
import type { Workflow, WorkflowListing } from "./types";

const draft = (extra: Partial<Draft> = {}): Draft => ({
  id: "",
  label: "Feature",
  description: "",
  scope: "user",
  origin: null,
  phases: [
    { skill: "plan", when: "", next: ["build"] },
    { skill: "build", when: "", next: ["review", "plan"] },
    { skill: "review", when: "", next: [] },
  ],
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
    expect(problem(draft({ phases: [{ skill: " ", when: "", next: [] }] }))).toMatch(/Phase 1 has no skill/);
    expect(problem(draft({ phases: [{ skill: "a b", when: "", next: [] }] }))).toMatch(/isn't a skill name/);
    expect(problem(draft({ phases: [{ skill: "a", when: "", next: [] }, { skill: "a", when: "", next: [] }] }))).toMatch(/twice/);
    expect(problem(blank("user"))).toMatch(/name/);
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

  it("keeps links when a skill is renamed", () => {
    const d = draft();
    renameSkill(d, 1, "make");
    expect(d.phases[0].next).toEqual(["make"]);
    expect(d.phases[1].skill).toBe("make");
    expect(d.phases[1].next).toEqual(["review", "plan"]);
    // A phase with no skill yet has nothing pointing at it.
    const e = draft({ phases: [{ skill: "", when: "", next: [] }, { skill: "x", when: "", next: [""] }] });
    renameSkill(e, 0, "y");
    expect(e.phases[1].next).toEqual([""]);
  });

  it("leaves links alone while another phase has the same skill", () => {
    const d = draft({ phases: [{ skill: "a", when: "", next: ["b"] }, { skill: "b", when: "", next: [] }, { skill: "b", when: "", next: [] }] });
    renameSkill(d, 1, "c");
    expect(d.phases[0].next).toEqual(["b"]);
    const dup = draft({ phases: [{ skill: "a", when: "", next: ["b"] }, { skill: "b", when: "", next: [] }, { skill: "b", when: "", next: [] }] });
    removePhase(dup, 2);
    expect(dup.phases[0].next).toEqual(["b"]);
    const e = draft({ phases: [{ skill: "a", when: "", next: ["b"] }, { skill: "b ", when: "", next: [] }] });
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

  it("removes a phase and every way to it", () => {
    const d = draft();
    removePhase(d, 1);
    expect(d.phases.map((p) => p.skill)).toEqual(["plan", "review"]);
    expect(d.phases[0].next).toEqual([]);
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
    const w: Workflow = { id: "f", label: "F", description: "d", phases: [{ skill: "a", when: "w", next: ["a"] }] };
    const d = draftOf(w, true);
    expect(d.scope).toBe("project");
    expect(d.origin).toBe("project");
    expect(draftOf(w, false).scope).toBe("user");
    d.phases[0].next.push("z");
    expect(w.phases[0].next).toEqual(["a"]);
  });

  it("offers installed skills no phase uses yet", () => {
    const skills = [
      { name: "plan", description: "", source: "project" as const },
      { name: "ship", description: "", source: "user" as const },
    ];
    expect(unusedSkills(draft(), skills).map((s) => s.name)).toEqual(["ship"]);
  });
});

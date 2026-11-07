// @vitest-environment jsdom
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { WorkflowEditor } from "./workfloweditor";
import type { Daemon } from "./daemon";
import type { Workflow, WorkflowListing } from "./types";

const listing: WorkflowListing = {
  workflows: [{ id: "feature", label: "Feature", description: "", phases: [{ skill: "plan", when: "", next: [] }] }],
  from_project: ["feature"],
  problems: [],
  skills: [
    { name: "plan", description: "Plan it", source: "project" },
    { name: "ship", description: "", source: "user" },
  ],
};
const el = <T extends HTMLElement>(id: string) => document.getElementById(id) as T;
const type = (input: HTMLInputElement, value: string) => {
  input.value = value;
  input.dispatchEvent(new Event("input", { bubbles: true }));
  input.dispatchEvent(new Event("change", { bubbles: true }));
};

describe("workflow editor dialog", () => {
  let saved: { scope: string; workflow: Workflow }[];
  let deleted: { scope: string; id: string }[];
  let editor: WorkflowEditor;
  let onSaved: ReturnType<typeof vi.fn>;
  beforeEach(() => {
    const html = readFileSync(resolve(process.cwd(), "index.html"), "utf8");
    document.body.innerHTML = /<body[^>]*>([\s\S]*)<\/body>/.exec(html)![1].replace(/<script[\s\S]*?<\/script>/g, "");
    HTMLDialogElement.prototype.showModal = function () { this.setAttribute("open", ""); };
    HTMLDialogElement.prototype.close = function () { this.removeAttribute("open"); };
    saved = [];
    deleted = [];
    const daemon = {
      saveWorkflow: async (_h: string, _d: string, scope: string, workflow: Workflow) => (saved.push({ scope, workflow }), null),
      deleteWorkflow: async (_h: string, _d: string, scope: string, id: string) => (deleted.push({ scope, id }), null),
    } as unknown as Daemon;
    editor = new WorkflowEditor(daemon);
    onSaved = vi.fn();
  });

  it("makes a new workflow from skills and saves it", async () => {
    editor.open("win", "C:/p", listing, "", onSaved);
    expect(el("wf-editor").hasAttribute("open")).toBe(true);
    expect(el<HTMLSelectElement>("wf-scope").value).toBe("project");
    type(el("wf-label"), "Ship it");
    type(document.querySelector<HTMLInputElement>('[data-i="0"][data-f="skill"]')!, "plan");
    el("wf-add").click();
    type(document.querySelector<HTMLInputElement>('[data-i="1"][data-f="skill"]')!, "ship");
    // Phase 1 can be followed by phase 2.
    const chip = document.querySelector<HTMLInputElement>('[data-i="0"][data-f="next"][value="ship"]')!;
    chip.checked = true;
    chip.dispatchEvent(new Event("change", { bubbles: true }));
    el("wf-form").dispatchEvent(new Event("submit", { cancelable: true }));
    await vi.waitFor(() => expect(saved).toHaveLength(1));
    expect(saved[0].scope).toBe("project");
    expect(saved[0].workflow).toEqual({ id: "ship-it", label: "Ship it", description: "", phases: [{ skill: "plan", instruction: "", when: "", next: ["ship"] }, { skill: "ship", instruction: "", when: "", next: [] }], loops: [] });
    await vi.waitFor(() => expect(onSaved).toHaveBeenCalledWith("ship-it"));
    expect(el("wf-editor").hasAttribute("open")).toBe(false);
  });

  it("doesn't save on Enter, and keeps its buttons when a skill name is settled", async () => {
    editor.open("win", "C:/p", listing, "feature", onSaved);
    const skill = document.querySelector<HTMLInputElement>('[data-i="0"][data-f="skill"]')!;
    const enter = new KeyboardEvent("keydown", { key: "Enter", bubbles: true, cancelable: true });
    skill.dispatchEvent(enter);
    expect(enter.defaultPrevented).toBe(true);
    const remove = document.querySelector('[data-act="remove"]');
    type(skill, "ship");
    expect(document.querySelector('[data-act="remove"]')).toBe(remove);
    expect(saved).toEqual([]);
  });

  it("won't replace an existing workflow by naming a new one the same", async () => {
    editor.open("win", "C:/p", listing, "", onSaved);
    type(el("wf-label"), "Feature");
    type(document.querySelector<HTMLInputElement>('[data-i="0"][data-f="skill"]')!, "plan");
    el("wf-form").dispatchEvent(new Event("submit", { cancelable: true }));
    expect(el("wf-error").textContent).toMatch(/already a workflow/);
    expect(saved).toEqual([]);
  });

  it("renames a phase once, from the name it started with, however the typing passes other names", () => {
    const l: WorkflowListing = { ...listing, workflows: [{ id: "t", label: "T", description: "", phases: [{ skill: "x", when: "", next: ["xy"] }, { skill: "xy", when: "", next: [] }] }], from_project: [] };
    editor.open("win", "C:/p", l, "t", onSaved);
    const second = document.querySelector<HTMLInputElement>('[data-i="1"][data-f="skill"]')!;
    second.dispatchEvent(new FocusEvent("focusin", { bubbles: true }));
    for (const v of ["x", "xz"]) {
      second.value = v;
      second.dispatchEvent(new Event("input", { bubbles: true }));
    }
    second.dispatchEvent(new Event("change", { bubbles: true }));
    el("wf-form").dispatchEvent(new Event("submit", { cancelable: true }));
    return vi.waitFor(() => {
      expect(saved).toHaveLength(1);
      expect(saved[0].workflow.phases.map((p) => [p.skill, p.next])).toEqual([["x", ["xz"]], ["xz", []]]);
    });
  });

  it("deletes from the folder the workflow was loaded from, whatever Save for says now", async () => {
    editor.open("win", "C:/p", listing, "feature", onSaved);
    const scope = el<HTMLSelectElement>("wf-scope");
    scope.value = "user";
    scope.dispatchEvent(new Event("change", { bubbles: true }));
    vi.stubGlobal("confirm", () => true);
    el("wf-delete").click();
    await vi.waitFor(() => expect(deleted).toEqual([{ scope: "project", id: "feature" }]));
    await vi.waitFor(() => expect(onSaved).toHaveBeenCalledWith(null));
    vi.unstubAllGlobals();
  });

  it("builds investigate, triage, a develop-test-review loop, then publish", async () => {
    editor.open("win", "C:/p", listing, "", onSaved);
    type(el("wf-label"), "Bug fix");
    const phase = (i: number, f: string) => document.querySelector<HTMLInputElement>(`[data-i="${i}"][data-f="${f}"]`)!;
    const names = ["investigate", "triage", "develop", "test", "review", "publish"];
    names.forEach((n, i) => {
      if (i > 0) el("wf-add").click();
      phase(i, "skill").dispatchEvent(new FocusEvent("focusin", { bubbles: true }));
      type(phase(i, "skill"), n);
    });
    // triage is a custom phase with its own instruction.
    const kind = document.querySelector<HTMLSelectElement>('[data-i="1"][data-f="kind"]')!;
    kind.value = "custom";
    kind.dispatchEvent(new Event("change", { bubbles: true }));
    expect(phase(1, "skill").placeholder).toMatch(/Phase name/);
    expect(document.querySelector('[data-i="1"][data-f="skill"]')!.getAttribute("list")).toBeNull();
    expect(document.querySelector('[data-i="2"][data-f="skill"]')!.getAttribute("list")).toBe("wf-skills");
    type(document.querySelector<HTMLTextAreaElement>('[data-i="1"][data-f="instruction"]') as unknown as HTMLInputElement, "Decide how bad it is.");
    // A loop over develop to review.
    el("wf-add-loop").click();
    const choose = (f: string, v: string) => {
      const sel = document.querySelector<HTMLSelectElement>(`[data-l="0"][data-f="${f}"]`)!;
      sel.value = v;
      sel.dispatchEvent(new Event("change", { bubbles: true }));
    };
    choose("from", "develop");
    choose("to", "review");
    type(document.querySelector<HTMLInputElement>('[data-l="0"][data-f="until"]')!, "the review finds nothing");
    type(document.querySelector<HTMLInputElement>('[data-l="0"][data-f="max"]')!, "4");
    el("wf-form").dispatchEvent(new Event("submit", { cancelable: true }));
    await vi.waitFor(() => expect(saved).toHaveLength(1));
    const w = saved[0].workflow;
    expect(w.phases.map((p) => p.skill)).toEqual(names);
    expect(w.phases.map((p) => p.instruction)).toEqual(["", "Decide how bad it is.", "", "", "", ""]);
    expect(w.loops).toEqual([{ from: "develop", to: "review", until: "the review finds nothing", max: 4 }]);
  });

  it("refuses a loop that ends before it starts, and drops loops with a phase that's removed", () => {
    const l: WorkflowListing = {
      ...listing,
      workflows: [{ id: "b", label: "B", description: "", phases: [{ skill: "a", when: "", next: [] }, { skill: "b", when: "", next: [] }, { skill: "c", when: "", next: [] }], loops: [{ from: "c", to: "b", until: "", max: 3 }] }],
      from_project: [],
    };
    editor.open("win", "C:/p", l, "b", onSaved);
    el("wf-form").dispatchEvent(new Event("submit", { cancelable: true }));
    expect(el("wf-error").textContent).toMatch(/ends before it starts/);
    expect(saved).toEqual([]);
    document.querySelector<HTMLButtonElement>('[data-act="remove-loop"]')!.click();
    expect(document.querySelectorAll(".wf-loop-row")).toHaveLength(0);
  });

  it("explains a draft that can't be saved, without calling the daemon", () => {
    editor.open("wsl:Ubuntu", "/home/p", listing, "", onSaved);
    expect(el<HTMLSelectElement>("wf-scope").value).toBe("user");
    el("wf-form").dispatchEvent(new Event("submit", { cancelable: true }));
    expect(el("wf-error").textContent).toMatch(/name/);
    expect(saved).toEqual([]);
  });

  it("opens an existing workflow with a delete button, and a new one without", () => {
    editor.open("win", "C:/p", listing, "feature", onSaved);
    expect(el<HTMLInputElement>("wf-label").value).toBe("Feature");
    expect(el("wf-delete").hidden).toBe(false);
    expect(document.querySelector<HTMLInputElement>('[data-i="0"][data-f="skill"]')!.value).toBe("plan");
    editor.open("win", "C:/p", listing, "", onSaved);
    expect(el("wf-delete").hidden).toBe(true);
  });

  it("suggests only skills no phase uses, and warns about one that isn't installed", () => {
    editor.open("win", "C:/p", listing, "feature", onSaved);
    expect([...document.querySelectorAll("#wf-skills option")].map((o) => (o as HTMLOptionElement).value)).toEqual(["ship"]);
    type(document.querySelector<HTMLInputElement>('[data-i="0"][data-f="skill"]')!, "nope");
    expect(el("wf-phases").textContent).toContain('No skill named "nope" is installed');
  });
});

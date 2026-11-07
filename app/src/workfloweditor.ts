// Workflow editor: make or change a workflow by picking skills for its phases.
// The pure part (a draft, its checks, turning it into a workflow) is tested;
// the dialog below it only draws the draft and edits it.

import type { Daemon } from "./daemon";
import type { SkillInfo, Workflow, WorkflowListing } from "./types";

const esc = (s: string) => s.replace(/[&<>"']/g, (c) => `&#${c.charCodeAt(0)};`);

export type Scope = "project" | "user";

export interface DraftPhase {
  skill: string;
  when: string;
  next: string[];
}

export interface Draft {
  /** Set for an existing workflow; a new one gets its id from its name when saved. */
  id: string;
  label: string;
  description: string;
  /** Where the workflow is saved from now on. */
  scope: Scope;
  /** The folder an existing workflow was loaded from (what Delete removes); null for a new one. */
  origin: Scope | null;
  phases: DraftPhase[];
}

const SKILL_CHARS = /^[A-Za-z0-9._:-]{1,64}$/;

export function blank(scope: Scope): Draft {
  return { id: "", label: "", description: "", scope, origin: null, phases: [{ skill: "", when: "", next: [] }] };
}

/** An existing workflow as a draft; `fromProject` says where it was found. */
export function draftOf(w: Workflow, fromProject: boolean): Draft {
  return { id: w.id, label: w.label, description: w.description, scope: fromProject ? "project" : "user", origin: fromProject ? "project" : "user", phases: w.phases.map((p) => ({ skill: p.skill, when: p.when, next: [...p.next] })) };
}

/** "Feature work!" -> "feature-work": the file name a new workflow gets. */
export function slug(label: string): string {
  return label
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-+|-+$/g, "")
    .slice(0, 48);
}

/** What stops this draft being saved, in words for the person; null when nothing does. */
export function problem(d: Draft): string | null {
  if (!d.label.trim()) return "Give the workflow a name.";
  if (!d.id && !slug(d.label)) return "Name it with some letters or digits.";
  if (d.phases.length === 0) return "Add at least one phase.";
  const seen = new Set<string>();
  for (const [i, p] of d.phases.entries()) {
    const skill = p.skill.trim();
    if (!skill) return `Phase ${i + 1} has no skill. Pick one or remove the phase.`;
    if (!SKILL_CHARS.test(skill)) return `"${skill}" isn't a skill name: use letters, digits, - _ . and :.`;
    if (seen.has(skill)) return `"${skill}" is a phase twice. A phase can be followed by itself or by an earlier one instead.`;
    seen.add(skill);
  }
  return null;
}

/** The workflow a valid draft stands for; `next` entries for phases that are gone are dropped. */
export function toWorkflow(d: Draft): Workflow {
  const skills = new Set(d.phases.map((p) => p.skill.trim()));
  return {
    id: d.id || slug(d.label),
    label: d.label.trim(),
    description: d.description.trim(),
    phases: d.phases.map((p) => ({ skill: p.skill.trim(), when: p.when.trim(), next: p.next.filter((n) => skills.has(n)) })),
  };
}

/** Rename a phase's skill, keeping the phases that lead to it pointing at it. */
export function renameSkill(d: Draft, i: number, to: string, was: string = d.phases[i].skill): void {
  const from = was.trim();
  d.phases[i].skill = to;
  const now = to.trim();
  if (!from || from === now) return;
  // Another phase still has the old name: the links were to that one.
  if (d.phases.some((q, j) => j !== i && q.skill.trim() === from)) return;
  for (const p of d.phases) p.next = p.next.map((n) => (n === from ? now : n));
}

/** Remove phase `i` and every way to it. */
export function removePhase(d: Draft, i: number): void {
  const [gone] = d.phases.splice(i, 1);
  const name = gone.skill.trim();
  // Another phase with the same skill keeps the links.
  if (d.phases.some((q) => q.skill.trim() === name)) return;
  for (const p of d.phases) p.next = p.next.filter((n) => n !== name);
}

/** Why saving this draft would do something unintended given what already exists; null when nothing. */
export function clash(d: Draft, l: WorkflowListing): string | null {
  const id = d.id || slug(d.label);
  if (!d.id && l.workflows.some((w) => w.id === id)) {
    return `There is already a workflow called "${l.workflows.find((w) => w.id === id)!.label}". Pick it in the Edit list to change it, or give this one another name.`;
  }
  if (d.scope === "user" && l.from_project.includes(id)) {
    return "This project has its own workflow with this name, and it would win over one saved for you. Save it for this project, or rename it.";
  }
  return null;
}

export function movePhase(d: Draft, i: number, by: -1 | 1): void {
  const j = i + by;
  if (j < 0 || j >= d.phases.length) return;
  [d.phases[i], d.phases[j]] = [d.phases[j], d.phases[i]];
}

/** Skills installed for the project or the user whose names aren't any phase's yet. */
export function unusedSkills(d: Draft, skills: SkillInfo[]): SkillInfo[] {
  const used = new Set(d.phases.map((p) => p.skill.trim()));
  return skills.filter((s) => !used.has(s.name));
}

export class WorkflowEditor {
  private dlg = document.getElementById("wf-editor") as HTMLDialogElement;
  private pick = document.getElementById("wf-pick") as HTMLSelectElement;
  private label = document.getElementById("wf-label") as HTMLInputElement;
  private description = document.getElementById("wf-description") as HTMLInputElement;
  private scope = document.getElementById("wf-scope") as HTMLSelectElement;
  private phases = document.getElementById("wf-phases")!;
  private skillList = document.getElementById("wf-skills") as HTMLDataListElement;
  private error = document.getElementById("wf-error")!;
  private save = document.getElementById("wf-save") as HTMLButtonElement;
  private del = document.getElementById("wf-delete") as HTMLButtonElement;
  private draft: Draft = blank("user");
  /** The skill name in the field being edited when it gained focus: links are moved once, from it to the final name. */
  private skillWas: { i: number; name: string } | null = null;
  private listing: WorkflowListing = { workflows: [], from_project: [], problems: [], skills: [] };
  private host = "";
  private dir = "";
  private onSaved: (id: string | null) => void = () => {};

  constructor(private daemon: Daemon) {
    this.pick.addEventListener("change", () => {
      const w = this.listing.workflows.find((x) => x.id === this.pick.value);
      this.draft = w ? draftOf(w, this.listing.from_project.includes(w.id)) : blank(this.draft.scope);
      this.show();
    });
    this.label.addEventListener("input", () => (this.draft.label = this.label.value));
    this.description.addEventListener("input", () => (this.draft.description = this.description.value));
    this.scope.addEventListener("change", () => (this.draft.scope = this.scope.value as Scope));
    document.getElementById("wf-add")!.addEventListener("click", () => {
      this.draft.phases.push({ skill: "", when: "", next: [] });
      this.drawPhases();
      this.phases.querySelector<HTMLInputElement>(`[data-i="${this.draft.phases.length - 1}"][data-f="skill"]`)?.focus();
    });
    this.phases.addEventListener("focusin", (e) => {
      const t = e.target as HTMLInputElement;
      if (t.dataset.f === "skill") this.skillWas = { i: Number(t.dataset.i), name: this.draft.phases[Number(t.dataset.i)]?.skill ?? "" };
    });
    this.phases.addEventListener("input", (e) => this.edit(e.target as HTMLInputElement, false));
    this.phases.addEventListener("change", (e) => this.edit(e.target as HTMLInputElement, true));
    this.phases.addEventListener("click", (e) => {
      const b = (e.target as HTMLElement).closest<HTMLButtonElement>("button[data-act]");
      if (!b) return;
      const i = Number(b.dataset.i);
      if (b.dataset.act === "up") movePhase(this.draft, i, -1);
      else if (b.dataset.act === "down") movePhase(this.draft, i, 1);
      else removePhase(this.draft, i);
      this.drawPhases();
    });
    // Enter in a field moves on; saving is the Save button's job.
    this.phases.addEventListener("keydown", (e) => {
      if (e.key === "Enter" && (e.target as HTMLElement).tagName === "INPUT") e.preventDefault();
    });
    for (const input of [this.label, this.description]) input.addEventListener("keydown", (e) => e.key === "Enter" && e.preventDefault());
    document.getElementById("wf-cancel")!.addEventListener("click", () => this.dlg.close());
    document.getElementById("wf-form")!.addEventListener("submit", (e) => {
      e.preventDefault();
      void this.submit();
    });
    this.del.addEventListener("click", () => void this.remove());
  }

  /** Open for the folder and host the New session dialog has; `onSaved` gets the saved id (or null after a delete). */
  open(host: string, dir: string, listing: WorkflowListing, select: string, onSaved: (id: string | null) => void): void {
    this.host = host;
    this.dir = dir;
    this.onSaved = onSaved;
    this.setListing(listing);
    const w = listing.workflows.find((x) => x.id === select);
    this.draft = w ? draftOf(w, listing.from_project.includes(w.id)) : blank(host === "win" ? "project" : "user");
    this.show();
    this.dlg.showModal();
    this.label.focus();
  }

  private setListing(l: WorkflowListing): void {
    this.listing = l;
    this.pick.innerHTML = "";
    this.pick.add(new Option("New workflow", ""));
    for (const w of l.workflows) this.pick.add(new Option(l.from_project.includes(w.id) ? `${w.label} (this project)` : w.label, w.id));

  }

  private show(): void {
    this.error.textContent = "";
    this.pick.value = this.draft.id;
    this.label.value = this.draft.label;
    this.description.value = this.draft.description;
    this.scope.value = this.draft.scope;
    this.del.hidden = !this.draft.id;
    this.drawPhases();
  }

  /** The phase rows: skill, when to use it, which phases can follow, and order controls. */
  private drawPhases(): void {
    const d = this.draft;
    this.phases.innerHTML = d.phases
      .map((p, i) => {
        return `<div class="wf-row">
          <div class="wf-row-top">
            <span class="wf-num">${i + 1}</span>
            <input type="text" list="wf-skills" data-i="${i}" data-f="skill" value="${esc(p.skill)}" placeholder="Skill name" aria-label="Skill for phase ${i + 1}" />
            <button type="button" data-act="up" data-i="${i}" title="Move up" aria-label="Move phase ${i + 1} up"${i === 0 ? " disabled" : ""}>↑</button>
            <button type="button" data-act="down" data-i="${i}" title="Move down" aria-label="Move phase ${i + 1} down"${i === d.phases.length - 1 ? " disabled" : ""}>↓</button>
            <button type="button" data-act="remove" data-i="${i}" title="Remove phase" aria-label="Remove phase ${i + 1}">✕</button>
          </div>
          <div data-slot="warn" data-i="${i}"></div>
          <input type="text" data-i="${i}" data-f="when" value="${esc(p.when)}" placeholder="Use when… (e.g. the change isn't clear yet)" aria-label="When to use phase ${i + 1}" />
          <div class="wf-next small" data-slot="next" data-i="${i}"></div>
        </div>`;
      })
      .join("");
    this.drawExtras();
  }

  /** The parts of each row that follow the skill names: warnings and "followed by" choices. Redrawn in place, so a click on a button next to a field just edited isn't lost. */
  private drawExtras(): void {
    const installed = new Set(this.listing.skills.map((s) => s.name));
    const d = this.draft;
    const names = [...new Set(d.phases.map((q) => q.skill.trim()).filter(Boolean))];
    this.skillList.innerHTML = unusedSkills(d, this.listing.skills)
      .map((s) => `<option value="${esc(s.name)}">${esc(`${s.source}${s.description ? ` · ${s.description}` : ""}`)}</option>`)
      .join("");
    d.phases.forEach((p, i) => {
      const skill = p.skill.trim();
      const warn = this.phases.querySelector<HTMLElement>(`[data-slot="warn"][data-i="${i}"]`);
      const next = this.phases.querySelector<HTMLElement>(`[data-slot="next"][data-i="${i}"]`);
      if (warn) warn.innerHTML = skill && !installed.has(skill) && !skill.includes(":") ? `<p class="small warn">No skill named "${esc(skill)}" is installed for this project or you.</p>` : "";
      if (next) {
        const chips = names.map((n) => `<label class="wf-chip"><input type="checkbox" data-i="${i}" data-f="next" value="${esc(n)}"${p.next.includes(n) ? " checked" : ""} />${esc(n)}</label>`).join("");
        next.innerHTML = `<span class="muted">Usually followed by</span>${chips || `<span class="muted"> (add more phases)</span>`}`;
      }
    });
  }

  private edit(el: HTMLInputElement, committed: boolean): void {
    const i = Number(el.dataset.i);
    const f = el.dataset.f;
    const p = this.draft.phases[i];
    if (!p || !f) return;
    if (f === "when") p.when = el.value;
    else if (f === "skill") {
      // Typing only sets the name. Links move once it's settled, from the name the field started with,
      // so passing through another phase's name on the way doesn't take them there.
      if (!committed) p.skill = el.value;
      else {
        const was = this.skillWas?.i === i ? this.skillWas.name : p.skill;
        p.skill = was;
        renameSkill(this.draft, i, el.value, was);
        this.skillWas = { i, name: el.value };
        this.drawExtras();
      }
    } else if (f === "next" && committed) {
      p.next = el.checked ? [...new Set([...p.next, el.value])] : p.next.filter((n) => n !== el.value);
    }
  }

  private async submit(): Promise<void> {
    const why = problem(this.draft) ?? clash(this.draft, this.listing);
    if (why) {
      this.error.textContent = why;
      return;
    }
    this.save.disabled = true;
    this.error.textContent = "";
    try {
      const w = toWorkflow(this.draft);
      const err = await this.daemon.saveWorkflow(this.host, this.dir, this.draft.scope, w);
      if (err) {
        this.error.textContent = err;
        return;
      }
      this.dlg.close();
      this.onSaved(w.id);
    } finally {
      this.save.disabled = false;
    }
  }

  private async remove(): Promise<void> {
    if (!this.draft.id || !confirm(`Delete the workflow "${this.draft.label}"? Sessions already running it are not affected.`)) return;
    const err = await this.daemon.deleteWorkflow(this.host, this.dir, this.draft.origin ?? this.draft.scope, this.draft.id);
    if (err) {
      this.error.textContent = err;
      return;
    }
    this.dlg.close();
    this.onSaved(null);
  }
}

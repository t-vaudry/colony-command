// Workflow editor: make or change a workflow. A phase is a skill or a custom
// instruction, and a run of phases can be a loop. The pure part (a draft, its
// checks, turning it into a workflow) is tested; the dialog below it only draws
// the draft and edits it.

import type { Daemon } from "./daemon";
import type { SkillInfo, Workflow, WorkflowListing } from "./types";

const esc = (s: string) => s.replace(/[&<>"']/g, (c) => `&#${c.charCodeAt(0)};`);

export type Scope = "project" | "user";

/** "If `condition`, go to `to`." */
export interface DraftBranch {
  condition: string;
  to: string;
}

export interface DraftPhase {
  /** The skill's name; for a custom phase just its name. */
  skill: string;
  /** A custom phase does what `instruction` says instead of running a skill. */
  custom: boolean;
  instruction: string;
  when: string;
  /** When the phase may be skipped; "" means it always runs. */
  skipWhen: string;
  /** The user approves before the phase starts. */
  approve: boolean;
  /** Only when this holds; "" means always. */
  approveWhen: string;
  /** Where a "no" goes; "" ends the run. */
  onDeny: string;
  next: string[];
  /** Ways out by condition; with these (or an `otherwise`) `next` is not used. */
  branches: DraftBranch[];
  /** Where it goes when no condition holds; "" for nowhere in particular. */
  otherwise: string;
}

export interface DraftLoop {
  from: string;
  to: string;
  until: string;
  max: number;
}

/** Phases `from` to `to`, in the order listed, that run at the same time. */
export interface DraftGroup {
  from: string;
  to: string;
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
  loops: DraftLoop[];
  parallel: DraftGroup[];
}

const SKILL_CHARS = /^[A-Za-z0-9._:-]{1,64}$/;
const DEFAULT_ROUNDS = 3;
const MAX_ROUNDS = 20;
const MAX_MEMBERS = 6;

export const newPhase = (): DraftPhase => ({ skill: "", custom: false, instruction: "", when: "", skipWhen: "", approve: false, approveWhen: "", onDeny: "", next: [], branches: [], otherwise: "" });

/** Whether a phase leaves by conditions rather than a plain list of followers. */
export const branching = (p: DraftPhase): boolean => p.branches.length > 0 || p.otherwise !== "";

export function blank(scope: Scope): Draft {
  return { id: "", label: "", description: "", scope, origin: null, phases: [newPhase()], loops: [], parallel: [] };
}

/** An existing workflow as a draft; `fromProject` says where it was found. */
export function draftOf(w: Workflow, fromProject: boolean): Draft {
  const where: Scope = fromProject ? "project" : "user";
  return {
    id: w.id,
    label: w.label,
    description: w.description,
    scope: where,
    origin: where,
    phases: w.phases.map((p) => ({
      skill: p.skill,
      custom: !!p.instruction,
      instruction: p.instruction ?? "",
      when: p.when,
      skipWhen: p.skip_when ?? "",
      approve: !!p.approve,
      approveWhen: p.approve_when ?? "",
      onDeny: p.on_deny ?? "",
      next: [...p.next],
      branches: (p.branches ?? []).filter((b) => b.condition).map((b) => ({ condition: b.condition, to: b.to })),
      otherwise: (p.branches ?? []).find((b) => !b.condition)?.to ?? "",
    })),
    loops: (w.loops ?? []).map((l) => ({ ...l })),
    parallel: (w.parallel ?? []).filter((g) => g.phases.length > 1).map((g) => ({ from: g.phases[0], to: g.phases[g.phases.length - 1] })),
  };
}

/** "Feature work!" -> "feature-work": the file name a new workflow gets. */
export function slug(label: string): string {
  return label
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-+|-+$/g, "")
    .slice(0, 48);
}

/** The names of the phases, in order, that have one. */
export function names(d: Draft): string[] {
  return d.phases.map((p) => p.skill.trim()).filter(Boolean);
}

/** What is wrong with the loops of a draft, in words for the person; null when nothing. */
export function loopProblem(d: Draft): string | null {
  const all = names(d);
  const spans: [number, number][] = [];
  for (const [k, l] of d.loops.entries()) {
    const n = k + 1;
    const a = all.indexOf(l.from.trim());
    const b = all.indexOf(l.to.trim());
    if (!l.from || !l.to) return `Loop ${n}: choose the first and last phase it repeats.`;
    if (a < 0 || b < 0) return `Loop ${n} names a phase that isn't in the list.`;
    if (b < a) return `Loop ${n} ends before it starts: "${l.to}" is earlier in the list than "${l.from}". Reorder the phases or swap them.`;
    if (!Number.isInteger(l.max) || l.max < 1 || l.max > MAX_ROUNDS) return `Loop ${n}: choose between 1 and ${MAX_ROUNDS} rounds.`;
    for (const [c, e] of spans) {
      const apart = b < c || e < a;
      const inside = (c <= a && b <= e) || (a <= c && e <= b);
      if (!apart && (!inside || (a === c && b === e))) return `Loop ${n} crosses or repeats another loop. Loops can sit inside one another or apart, not overlap.`;
    }
    spans.push([a, b]);
  }
  return null;
}

/** The index range of each group of a draft, or null where its ends aren't in the list in order. */
function groupSpans(d: Draft): ([number, number] | null)[] {
  const all = names(d);
  return d.parallel.map((g) => {
    const a = all.indexOf(g.from.trim());
    const b = all.indexOf(g.to.trim());
    return a < 0 || b <= a ? null : [a, b];
  });
}

/** What is wrong with the parallel groups of a draft, in words for the person; null when nothing. */
export function parallelProblem(d: Draft): string | null {
  const all = names(d);
  const spans: [number, number][] = [];
  for (const [k, g] of d.parallel.entries()) {
    const n = k + 1;
    const a = all.indexOf(g.from.trim());
    const b = all.indexOf(g.to.trim());
    if (!g.from || !g.to) return `Parallel group ${n}: choose the first and last phase that run together.`;
    if (a < 0 || b < 0) return `Parallel group ${n} names a phase that isn't in the list.`;
    if (b <= a) return `Parallel group ${n}: "${g.to}" has to come after "${g.from}" in the list, with at least two phases in the group.`;
    if (b - a + 1 > MAX_MEMBERS) return `Parallel group ${n} has more than ${MAX_MEMBERS} phases.`;
    if (spans.some(([c, e]) => !(b < c || e < a))) return `Parallel group ${n} overlaps another group.`;
    spans.push([a, b]);
  }
  for (const p of d.phases) {
    for (const t of [...p.next, ...p.branches.map((x) => x.to), p.otherwise]) {
      const j = all.indexOf(t);
      const i = all.indexOf(p.skill.trim());
      if (j >= 0 && spans.some(([a, b]) => a < j && j <= b && !(a <= i && i <= b))) return `Phase "${p.skill.trim()}" goes to "${t}", which runs in parallel with an earlier phase. Go to the first phase of the group instead.`;
    }
  }
  for (const [k, l] of d.loops.entries()) {
    const from = all.indexOf(l.from.trim());
    const to = all.indexOf(l.to.trim());
    if (spans.some(([a, b]) => (a < from && from <= b) || (a <= to && to < b))) return `Loop ${k + 1} starts or ends inside a parallel group. A loop has to hold a whole group.`;
  }
  return null;
}

/** What stops this draft being saved, in words for the person; null when nothing does. */
export function problem(d: Draft): string | null {
  if (!d.label.trim()) return "Give the workflow a name.";
  if (!d.id && !slug(d.label)) return "Name it with some letters or digits.";
  if (d.phases.length === 0) return "Add at least one phase.";
  const seen = new Set<string>();
  for (const [i, p] of d.phases.entries()) {
    const name = p.skill.trim();
    const what = p.custom ? "name" : "skill";
    if (!name) return p.custom ? `Phase ${i + 1} has no name. Name it or remove the phase.` : `Phase ${i + 1} has no skill. Pick one, make it a custom phase, or remove it.`;
    if (!SKILL_CHARS.test(name)) return `"${name}" isn't a ${what} name: use letters, digits, - _ . and :.`;
    if (p.custom && !p.instruction.trim()) return `Phase "${name}" is a custom phase but has no instruction. Say what to do, or make it a skill.`;
    if (seen.has(name)) return `"${name}" is a phase twice. A phase can be followed by itself or by an earlier one instead.`;
    seen.add(name);
  }
  const have = new Set(names(d));
  for (const p of d.phases) {
    const name = p.skill.trim();
    for (const b of p.branches) {
      if (!b.condition.trim()) return `Phase "${name}": a branch has no condition. Write when it applies, or remove it.`;
      if (!b.to) return `Phase "${name}": the branch "if ${b.condition.trim()}" doesn't say where it goes.`;
      if (!have.has(b.to)) return `Phase "${name}": the branch "if ${b.condition.trim()}" goes to "${b.to}", which isn't a phase.`;
    }
    if (p.otherwise && !have.has(p.otherwise)) return `Phase "${name}": "otherwise" goes to "${p.otherwise}", which isn't a phase.`;
  }
  return loopProblem(d) ?? parallelProblem(d);
}

/** The workflow a valid draft stands for; `next` entries for phases that are gone are dropped. */
export function toWorkflow(d: Draft): Workflow {
  const have = new Set(names(d));
  const spans = groupSpans(d);
  const groups = spans.flatMap((s) => (s ? [{ phases: names(d).slice(s[0], s[1] + 1) }] : []));
  return {
    id: d.id || slug(d.label),
    label: d.label.trim(),
    description: d.description.trim(),
    phases: d.phases.map((p, pi) => {
      const grouped = spans.find((s) => s && s[0] <= pi && pi <= s[1]);
      // In a parallel group only the last phase says where the run goes next, and no member is sent elsewhere on a denial.
      const notLast = !!grouped && pi < grouped[1];
      // Conditions in the order written, then the otherwise.
      const branches = notLast ? [] : [...p.branches.filter((b) => b.condition.trim() && have.has(b.to)).map((b) => ({ to: b.to, condition: b.condition.trim() })), ...(p.otherwise && have.has(p.otherwise) ? [{ to: p.otherwise, condition: "" }] : [])];
      return { skill: p.skill.trim(), instruction: p.custom ? p.instruction.trim() : "", when: p.when.trim(), skip_when: p.skipWhen.trim(), ...(p.approve ? { approve: true, approve_when: p.approveWhen.trim(), on_deny: !grouped && p.onDeny.trim() !== p.skill.trim() && have.has(p.onDeny.trim()) ? p.onDeny.trim() : "" } : {}), next: branches.length || notLast ? [] : p.next.filter((n) => have.has(n)), branches };
    }),
    loops: d.loops.filter((l) => have.has(l.from.trim()) && have.has(l.to.trim())).map((l) => ({ from: l.from.trim(), to: l.to.trim(), until: l.until.trim(), max: l.max })),
    ...(groups.length ? { parallel: groups } : {}),
  };
}

/** Rename a phase, keeping the phases and loops that point at it pointing at it. */
export function renameSkill(d: Draft, i: number, to: string, was: string = d.phases[i].skill): void {
  const from = was.trim();
  d.phases[i].skill = to;
  const now = to.trim();
  if (!from || from === now) return;
  // Another phase still has the old name: the links were to that one.
  if (d.phases.some((q, j) => j !== i && q.skill.trim() === from)) return;
  for (const p of d.phases) {
    p.next = p.next.map((n) => (n === from ? now : n));
    for (const b of p.branches) if (b.to === from) b.to = now;
    if (p.otherwise === from) p.otherwise = now;
    if (p.onDeny === from) p.onDeny = now;
  }
  for (const g of d.parallel) {
    if (g.from === from) g.from = now;
    if (g.to === from) g.to = now;
  }
  for (const l of d.loops) {
    if (l.from === from) l.from = now;
    if (l.to === from) l.to = now;
  }
}

/** Remove phase `i`, every way to it, and the loops that ran to or from it. */
export function removePhase(d: Draft, i: number): void {
  const [gone] = d.phases.splice(i, 1);
  const name = gone.skill.trim();
  // Another phase with the same name keeps the links.
  if (d.phases.some((q) => q.skill.trim() === name)) return;
  for (const p of d.phases) {
    p.next = p.next.filter((n) => n !== name);
    // A condition with nowhere to go is dropped with the phase it led to.
    p.branches = p.branches.filter((b) => b.to !== name);
    if (p.otherwise === name) p.otherwise = "";
    if (p.onDeny === name) p.onDeny = "";
  }
  d.loops = d.loops.filter((l) => l.from !== name && l.to !== name);
  d.parallel = d.parallel.filter((g) => g.from !== name && g.to !== name);
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

/** Skills installed for the project or the user that no skill phase uses yet. */
export function unusedSkills(d: Draft, skills: SkillInfo[]): SkillInfo[] {
  const used = new Set(d.phases.filter((p) => !p.custom).map((p) => p.skill.trim()));
  return skills.filter((s) => !used.has(s.name));
}

type Field = HTMLInputElement | HTMLTextAreaElement | HTMLSelectElement;

export class WorkflowEditor {
  private dlg = document.getElementById("wf-editor") as HTMLDialogElement;
  private pick = document.getElementById("wf-pick") as HTMLSelectElement;
  private label = document.getElementById("wf-label") as HTMLInputElement;
  private description = document.getElementById("wf-description") as HTMLInputElement;
  private scope = document.getElementById("wf-scope") as HTMLSelectElement;
  private phases = document.getElementById("wf-phases")!;
  private loops = document.getElementById("wf-loops")!;
  private parallel = document.getElementById("wf-parallel")!;
  private skillList = document.getElementById("wf-skills") as HTMLDataListElement;
  private error = document.getElementById("wf-error")!;
  private save = document.getElementById("wf-save") as HTMLButtonElement;
  private del = document.getElementById("wf-delete") as HTMLButtonElement;
  private draft: Draft = blank("user");
  /** The name in the field being edited when it gained focus: links are moved once, from it to the final name. */
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
      this.draft.phases.push(newPhase());
      this.drawPhases();
      this.phases.querySelector<HTMLInputElement>(`[data-i="${this.draft.phases.length - 1}"][data-f="skill"]`)?.focus();
    });
    document.getElementById("wf-add-loop")!.addEventListener("click", () => {
      this.draft.loops.push({ from: "", to: "", until: "", max: DEFAULT_ROUNDS });
      this.drawLoops();
    });
    document.getElementById("wf-add-parallel")!.addEventListener("click", () => {
      this.draft.parallel.push({ from: "", to: "" });
      this.drawParallel();
    });
    this.parallel.addEventListener("change", (e) => {
      const el = e.target as Field;
      const g = this.draft.parallel[Number(el.dataset.g)];
      if (g && (el.dataset.f === "from" || el.dataset.f === "to")) g[el.dataset.f] = el.value;
    });
    this.parallel.addEventListener("click", (e) => {
      const b = (e.target as HTMLElement).closest<HTMLButtonElement>("button[data-act='remove-parallel']");
      if (!b) return;
      this.draft.parallel.splice(Number(b.dataset.g), 1);
      this.drawParallel();
    });
    this.phases.addEventListener("focusin", (e) => {
      const t = e.target as HTMLInputElement;
      if (t.dataset.f === "skill") this.skillWas = { i: Number(t.dataset.i), name: this.draft.phases[Number(t.dataset.i)]?.skill ?? "" };
    });
    this.phases.addEventListener("input", (e) => this.edit(e.target as Field, false));
    this.phases.addEventListener("change", (e) => this.edit(e.target as Field, true));
    this.phases.addEventListener("click", (e) => {
      const b = (e.target as HTMLElement).closest<HTMLButtonElement>("button[data-act]");
      if (!b) return;
      const i = Number(b.dataset.i);
      const p = this.draft.phases[i];
      switch (b.dataset.act) {
        case "up":
          movePhase(this.draft, i, -1);
          break;
        case "down":
          movePhase(this.draft, i, 1);
          break;
        case "add-branch":
          // Going from plain followers to conditions: a single follower becomes the otherwise.
          if (!branching(p) && p.next.length === 1) p.otherwise = p.next[0];
          p.next = [];
          p.branches.push({ condition: "", to: "" });
          this.drawExtras();
          return;
        case "remove-branch":
          p.branches.splice(Number(b.dataset.b), 1);
          this.drawExtras();
          return;
        default:
          removePhase(this.draft, i);
      }
      this.drawPhases();
    });
    this.loops.addEventListener("input", (e) => this.editLoop(e.target as Field));
    this.loops.addEventListener("change", (e) => this.editLoop(e.target as Field));
    this.loops.addEventListener("click", (e) => {
      const b = (e.target as HTMLElement).closest<HTMLButtonElement>("button[data-act='remove-loop']");
      if (!b) return;
      this.draft.loops.splice(Number(b.dataset.l), 1);
      this.drawLoops();
    });
    // Enter in a field moves on; saving is the Save button's job.
    for (const box of [this.phases, this.loops]) {
      box.addEventListener("keydown", (e) => {
        if (e.key === "Enter" && (e.target as HTMLElement).tagName === "INPUT") e.preventDefault();
      });
    }
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

  /** The phase rows: skill or custom instruction, when to use it, which phases can follow, and order controls. */
  private drawPhases(): void {
    const d = this.draft;
    this.phases.innerHTML = d.phases
      .map((p, i) => {
        const n = i + 1;
        return `<div class="wf-row">
          <div class="wf-row-top">
            <span class="wf-num">${n}</span>
            <select data-i="${i}" data-f="kind" aria-label="Kind of phase ${n}">
              <option value="skill"${p.custom ? "" : " selected"}>Skill</option>
              <option value="custom"${p.custom ? " selected" : ""}>Custom</option>
            </select>
            <input type="text"${p.custom ? "" : ' list="wf-skills"'} data-i="${i}" data-f="skill" value="${esc(p.skill)}" placeholder="${p.custom ? "Phase name (e.g. triage)" : "Skill name"}" aria-label="${p.custom ? "Name" : "Skill"} for phase ${n}" />
            <button type="button" data-act="up" data-i="${i}" title="Move up" aria-label="Move phase ${n} up"${i === 0 ? " disabled" : ""}>↑</button>
            <button type="button" data-act="down" data-i="${i}" title="Move down" aria-label="Move phase ${n} down"${i === d.phases.length - 1 ? " disabled" : ""}>↓</button>
            <button type="button" data-act="remove" data-i="${i}" title="Remove phase" aria-label="Remove phase ${n}">✕</button>
          </div>
          <div data-slot="warn" data-i="${i}"></div>
          ${p.custom ? `<textarea data-i="${i}" data-f="instruction" rows="3" placeholder="What should Claude do in this phase?" aria-label="Instruction for phase ${n}">${esc(p.instruction)}</textarea>` : ""}
          <input type="text" data-i="${i}" data-f="when" value="${esc(p.when)}" placeholder="Use when… (e.g. the change isn't clear yet)" aria-label="When to use phase ${n}" />
          <input type="text" data-i="${i}" data-f="skipWhen" value="${esc(p.skipWhen)}" placeholder="Optional: skip when… (leave empty to always run)" aria-label="When phase ${n} may be skipped" />
          <div class="wf-next small" data-slot="next" data-i="${i}"></div>
          <div class="wf-approve small" data-slot="approve" data-i="${i}"></div>
        </div>`;
      })
      .join("");
    this.drawExtras();
  }

  /** The parts of each row that follow the names: warnings and "followed by" choices. Redrawn in place, so a click on a button next to a field just edited isn't lost. */
  private drawExtras(): void {
    const installed = new Set(this.listing.skills.map((s) => s.name));
    const d = this.draft;
    const all = [...new Set(names(d))];
    this.skillList.innerHTML = unusedSkills(d, this.listing.skills)
      .map((s) => `<option value="${esc(s.name)}">${esc(`${s.source}${s.description ? ` · ${s.description}` : ""}`)}</option>`)
      .join("");
    const spans = groupSpans(d);
    d.phases.forEach((p, i) => {
      const name = p.skill.trim();
      const group = spans.find((s) => s && s[0] <= i && i <= s[1]);
      const notLast = !!group && i < group[1];
      const warn = this.phases.querySelector<HTMLElement>(`[data-slot="warn"][data-i="${i}"]`);
      const next = this.phases.querySelector<HTMLElement>(`[data-slot="next"][data-i="${i}"]`);
      const approve = this.phases.querySelector<HTMLElement>(`[data-slot="approve"][data-i="${i}"]`);
      if (approve) {
        const others = all.filter((n) => n !== name);
        const deny = `<option value="">End the run</option>${others.map((n) => `<option value="${esc(n)}"${n === p.onDeny ? " selected" : ""}>Go to ${esc(n)}</option>`).join("")}`;
        approve.innerHTML = `<label class="wf-chip"><input type="checkbox" data-i="${i}" data-f="approve"${p.approve ? " checked" : ""} />Ask me before this phase starts</label>${
          p.approve
            ? ` <input type="text" data-i="${i}" data-f="approveWhen" value="${esc(p.approveWhen)}" placeholder="only when… (empty: always)" aria-label="When to ask before phase ${i + 1}" />${group ? ` <span class="muted">If I say no, it is left out.</span>` : ` <span class="muted">If I say no:</span> <select data-i="${i}" data-f="onDeny" aria-label="Where phase ${i + 1} goes if denied">${deny}</select>`}`
            : ""
        }`;
      }
      if (warn) warn.innerHTML = !p.custom && name && !installed.has(name) && !name.includes(":") ? `<p class="small warn">No skill named "${esc(name)}" is installed for this project or you.</p>` : "";
      if (next && notLast) {
        next.innerHTML = `<span class="muted">Runs together with the rest of its parallel group; the group's last phase says what comes next.</span>`;
      } else if (next && branching(p)) {
        const options = (chosen: string, none: string) => `<option value="">${none}</option>${all.map((n) => `<option value="${esc(n)}"${n === chosen ? " selected" : ""}>${esc(n)}</option>`).join("")}`;
        const rows = p.branches
          .map(
            (b, k) => `<div class="wf-branch-row"><span>if</span><input type="text" data-i="${i}" data-b="${k}" data-f="bcond" value="${esc(b.condition)}" placeholder="it is a regression" aria-label="Condition ${k + 1} of phase ${i + 1}" /><span>go to</span><select data-i="${i}" data-b="${k}" data-f="bto" aria-label="Where condition ${k + 1} of phase ${i + 1} goes">${options(b.to, "Choose…")}</select><button type="button" data-act="remove-branch" data-i="${i}" data-b="${k}" title="Remove condition" aria-label="Remove condition ${k + 1} of phase ${i + 1}">✕</button></div>`,
          )
          .join("");
        next.innerHTML = `<span class="muted">Then</span>${rows}<div class="wf-branch-row"><span>otherwise go to</span><select data-i="${i}" data-f="otherwise" aria-label="Where phase ${i + 1} goes otherwise">${options(p.otherwise, "Nowhere in particular")}</select><button type="button" data-act="add-branch" data-i="${i}">Add condition</button></div>`;
      } else if (next) {
        const chips = all.map((n) => `<label class="wf-chip"><input type="checkbox" data-i="${i}" data-f="next" value="${esc(n)}"${p.next.includes(n) ? " checked" : ""} />${esc(n)}</label>`).join("");
        const rest = p.next.length || i === d.phases.length - 1 ? "" : `<span class="muted"> · none ticked: the next phase in the list</span>`;
        next.innerHTML = `<span class="muted">Usually followed by</span>${chips || `<span class="muted"> (add more phases)</span>`}${rest} <button type="button" data-act="add-branch" data-i="${i}" title="Go to different phases depending on what happened">Branch on a condition…</button>`;
      }
    });
    this.drawLoops();
    this.drawParallel();
  }

  /** The parallel group rows: which run of phases goes side by side. */
  private drawParallel(): void {
    const all = [...new Set(names(this.draft))];
    const options = (chosen: string) => `<option value="">Choose…</option>${all.map((n) => `<option value="${esc(n)}"${n === chosen ? " selected" : ""}>${esc(n)}</option>`).join("")}`;
    this.parallel.innerHTML = this.draft.parallel
      .map(
        (g, k) => `<div class="wf-loop-row">
          <span class="wf-num">∥</span>
          <span>Run</span>
          <select data-g="${k}" data-f="from" aria-label="First phase of parallel group ${k + 1}">${options(g.from)}</select>
          <span>to</span>
          <select data-g="${k}" data-f="to" aria-label="Last phase of parallel group ${k + 1}">${options(g.to)}</select>
          <span>at the same time</span>
          <button type="button" data-act="remove-parallel" data-g="${k}" title="Remove group" aria-label="Remove parallel group ${k + 1}">✕</button>
        </div>`,
      )
      .join("");
  }

  /** The loop rows: which run of phases repeats, until what, and how many rounds at most. */
  private drawLoops(): void {
    const all = [...new Set(names(this.draft))];
    const options = (chosen: string) => `<option value="">Choose…</option>${all.map((n) => `<option value="${esc(n)}"${n === chosen ? " selected" : ""}>${esc(n)}</option>`).join("")}`;
    this.loops.innerHTML = this.draft.loops
      .map(
        (l, k) => `<div class="wf-loop-row">
          <span class="wf-num">↻</span>
          <span>Repeat</span>
          <select data-l="${k}" data-f="from" aria-label="First phase of loop ${k + 1}">${options(l.from)}</select>
          <span>to</span>
          <select data-l="${k}" data-f="to" aria-label="Last phase of loop ${k + 1}">${options(l.to)}</select>
          <span>until</span>
          <input type="text" data-l="${k}" data-f="until" value="${esc(l.until)}" placeholder="the review finds nothing" aria-label="Stop loop ${k + 1} when" />
          <span>at most</span>
          <input type="number" min="1" max="${MAX_ROUNDS}" data-l="${k}" data-f="max" value="${l.max}" aria-label="Most rounds for loop ${k + 1}" />
          <span>rounds</span>
          <button type="button" data-act="remove-loop" data-l="${k}" title="Remove loop" aria-label="Remove loop ${k + 1}">✕</button>
        </div>`,
      )
      .join("");
  }

  private editLoop(el: Field): void {
    const l = this.draft.loops[Number(el.dataset.l)];
    const f = el.dataset.f;
    if (!l || !f) return;
    if (f === "from" || f === "to" || f === "until") l[f] = el.value;
    else if (f === "max") l.max = Number(el.value);
  }

  private edit(el: Field, committed: boolean): void {
    const i = Number(el.dataset.i);
    const f = el.dataset.f;
    const p = this.draft.phases[i];
    if (!p || !f) return;
    if (f === "when") p.when = el.value;
    else if (f === "skipWhen") p.skipWhen = el.value;
    else if (f === "approveWhen") p.approveWhen = el.value;
    else if (f === "onDeny") p.onDeny = el.value;
    else if (f === "approve" && committed) {
      p.approve = (el as HTMLInputElement).checked;
      this.drawExtras();
    }
    else if (f === "instruction") p.instruction = el.value;
    else if (f === "bcond") p.branches[Number(el.dataset.b)].condition = el.value;
    else if (f === "bto") p.branches[Number(el.dataset.b)].to = el.value;
    else if (f === "otherwise") p.otherwise = el.value;
    else if (f === "kind") {
      if (committed) {
        p.custom = el.value === "custom";
        this.drawPhases();
      }
    } else if (f === "skill") {
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
      const box = el as HTMLInputElement;
      p.next = box.checked ? [...new Set([...p.next, box.value])] : p.next.filter((n) => n !== box.value);
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

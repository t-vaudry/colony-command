// "Set up Colony": installs the Claude Code hooks, the WSL probe and the
// approval hook, on Windows and in each WSL distro. Talks to the app's
// setup_* commands (crates/colony-setup); only works inside the desktop app.

type State = "missing" | "partial" | "outdated" | "changed" | "current";
type Sel = { hooks: boolean; probe: boolean; approval: boolean };

interface Component {
  id: keyof Sel;
  label: string;
  what: string;
  state: State;
}
interface TargetStatus {
  target: { id: string; label: string; kind: "windows" | "wsl"; running: boolean };
  checked: boolean;
  error: string | null;
  components: Component[];
  warnings: string[];
  missing_files: string[];
  settings_path: string | null;
}
interface Status {
  version: string;
  fingerprint: string;
  targets: TargetStatus[];
}
interface Plan {
  target: string;
  settings_path: string;
  diff: string;
  files: { path: string; action: string }[];
  backup_path: string | null;
  nothing: boolean;
  error: string | null;
  token: string;
}
interface Applied {
  target: string;
  ok: boolean;
  steps: { what: string; ok: boolean; detail: string | null }[];
}

type Invoke = <T>(cmd: string, args?: Record<string, unknown>) => Promise<T>;

const STATE_TEXT: Record<State, string> = {
  missing: "not installed",
  partial: "incomplete",
  outdated: "older version",
  changed: "modified",
  current: "up to date",
};
const DISMISSED_KEY = "colony.setup.dismissed";

function esc(s: string): string {
  return s.replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[c]!);
}

function invoker(): Invoke | null {
  const t = (window as unknown as { __TAURI__?: { core?: { invoke: Invoke } } }).__TAURI__;
  return t?.core?.invoke ?? null;
}

function needsAttention(s: TargetStatus): boolean {
  // Nothing to offer when the build lacks the files; the dialog explains that instead.
  return s.target.running && s.checked && s.missing_files.length === 0 && s.components.some((c) => c.state === "missing" || c.state === "partial" || c.state === "outdated" || c.state === "changed");
}

function store(key: string, value?: string): string | null {
  try {
    if (value !== undefined) localStorage.setItem(key, value);
    return localStorage.getItem(key);
  } catch {
    return null;
  }
}

export class SetupDialog {
  private invoke = invoker();
  private dlg = document.getElementById("setup") as HTMLDialogElement;
  private body = document.getElementById("setup-body")!;
  private btn = document.getElementById("setup-btn") as HTMLButtonElement;
  private status: Status | null = null;
  /** Targets included in this run, and what each should end up with. */
  private include = new Set<string>();
  private sel = new Map<string, Sel>();
  private stoppedRead = new Set<string>();
  private plans = new Map<string, Plan>();
  private busy = false;
  private autostart = false;

  constructor() {
    if (!this.invoke) {
      this.btn.hidden = true;
      return;
    }
    this.btn.addEventListener("click", () => void this.open());
    // Esc while files are being written would hide the window mid-apply.
    this.dlg.addEventListener("cancel", (e) => {
      if (this.busy) e.preventDefault();
      // Asked once per app version or hook build; the button stays for later.
      else if (this.status) store(DISMISSED_KEY, this.status.fingerprint);
    });
    this.dlg.addEventListener("close", () => void this.refresh());
    this.body.addEventListener("click", (e) => this.click(e));
    this.body.addEventListener("change", (e) => this.change(e));
    // First launch, and again after an update: offer once per app version or hook build.
    void this.refresh().then(() => {
      if (this.status && this.status.targets.some(needsAttention) && store(DISMISSED_KEY) !== this.status.fingerprint) void this.open();
    });
  }

  /** Re-reads what is installed and updates the button's badge. */
  private async refresh(): Promise<void> {
    try {
      this.status = await this.invoke!<Status>("setup_status", { includeStopped: [...this.stoppedRead] });
    } catch {
      return;
    }
    const broken = this.status.targets.find((t) => t.target.running && t.missing_files.length > 0);
    const attn = !!broken || this.status.targets.some(needsAttention);
    this.btn.classList.toggle("attn", attn);
    this.btn.title = broken ? `Set up can't run: ${broken.missing_files.join(", ")} missing from this build` : attn ? "Colony's hooks or probe aren't installed or are out of date" : "Install, repair or remove Colony's Claude Code hooks and WSL probe";
  }

  async open(): Promise<void> {
    await this.refresh();
    this.autostart = await this.invoke!<boolean>("autostart_enabled").catch(() => false);
    if (!this.status) return;
    this.include.clear();
    this.sel.clear();
    this.plans.clear();
    for (const t of this.status.targets) {
      if (t.target.running) this.include.add(t.target.id);
      // Start from what is installed: on a fresh target everything is ticked; on one that has
      // Colony, an item that isn't installed stays unticked (someone may have chosen that).
      const any = t.components.some((c) => c.state !== "missing");
      const on = (id: keyof Sel) => {
        const c = t.components.find((x) => x.id === id);
        return !!c && (!any || c.state !== "missing");
      };
      this.sel.set(t.target.id, { hooks: on("hooks"), probe: on("probe"), approval: on("approval") });
    }
    this.renderChoose();
    if (!this.dlg.open) this.dlg.showModal();
  }

  private close(dismiss: boolean): void {
    if (dismiss && this.status) store(DISMISSED_KEY, this.status.fingerprint);
    this.dlg.close();
  }

  // -- step 1: choose ------------------------------------------------------

  private renderChoose(): void {
    const st = this.status!;
    const targets = st.targets
      .map((t) => {
        const id = t.target.id;
        const on = this.include.has(id);
        const sel = this.sel.get(id)!;
        const stopped = !t.target.running && !this.stoppedRead.has(id);
        const rows = t.components
          .map(
            (c) => `<label class="su-comp" title="${esc(c.what)}">
              <input type="checkbox" data-comp="${esc(id)}|${c.id}" ${sel[c.id] ? "checked" : ""} ${on ? "" : "disabled"} />
              <span class="su-name">${esc(c.label)}</span>
              <span class="su-state ${c.state}">${STATE_TEXT[c.state]}</span>
              <span class="muted small su-what">${esc(c.what)}</span>
            </label>`,
          )
          .join("");
        return `<section class="su-target${on ? "" : " off"}">
          <div class="su-head">
            <label><input type="checkbox" data-inc="${esc(id)}" ${on ? "checked" : ""} /> <b>${esc(t.target.label)}</b></label>
            <span class="muted small">${t.target.running ? (t.target.kind === "wsl" ? "running" : "") : stopped ? "stopped" : "started"}</span>
          </div>
          ${stopped ? `<p class="muted small">This distro isn't running. Including it starts it so Colony can look inside.</p>` : ""}
          ${t.error ? `<p class="error">${esc(t.error)}</p>` : ""}
          ${t.missing_files.length ? `<p class="error"><b>Can't install:</b> ${esc(t.missing_files.join(", "))} ${t.missing_files.length > 1 ? "are" : "is"} missing from this build of Colony.</p>` : ""}
          ${t.warnings.map((w) => `<p class="${t.missing_files.length ? "error" : "su-warn"} small">${esc(w)}</p>`).join("")}
          ${rows}
        </section>`;
      })
      .join("");
    this.body.innerHTML = `
      <h2>Set up Colony</h2>
      <p class="muted">Colony learns about your Claude Code sessions through hooks in <span class="mono">~/.claude/settings.json</span>, and sees WSL sessions through a small probe in each distro. Nothing is changed until you have reviewed the exact edits on the next step. Your other settings are left as they are, and a timestamped backup is made first.</p>
      <div class="su-targets">${targets || `<p class="muted">No Windows or WSL targets found.</p>`}</div>
      <p class="muted small">Unchecking something that is already installed removes it.</p>
      <label class="su-comp" title="Starts Colony hidden in the tray when you sign in to Windows">
        <input type="checkbox" data-autostart ${this.autostart ? "checked" : ""} />
        <span class="su-name">Start at login</span>
        <span class="muted small su-what">Colony starts in the tray when you sign in. Applies right away, not part of Review changes.</span>
      </label>
      <div class="actions">
        <button type="button" class="primary" data-act="review">Review changes</button>
        <button type="button" data-act="later">Not now</button>
      </div>`;
  }

  private change(e: Event): void {
    const el = e.target as HTMLInputElement;
    if ("autostart" in el.dataset) {
      void this.invoke!<boolean>("autostart_set", { on: el.checked })
        .then((on) => (this.autostart = on))
        .catch(() => (this.autostart = !el.checked))
        .finally(() => this.renderChoose());
    } else if (el.dataset.inc) {
      const id = el.dataset.inc;
      if (el.checked) {
        this.include.add(id);
        const t = this.status!.targets.find((x) => x.target.id === id)!;
        if (!t.target.running && !this.stoppedRead.has(id)) {
          this.stoppedRead.add(id);
          void this.refresh().then(() => this.renderChoose());
          return;
        }
      } else {
        this.include.delete(id);
      }
      this.renderChoose();
    } else if (el.dataset.comp) {
      const [id, comp] = el.dataset.comp.split("|");
      this.sel.get(id)![comp as keyof Sel] = el.checked;
    }
  }

  private click(e: MouseEvent): void {
    const act = (e.target as HTMLElement).closest<HTMLElement>("[data-act]")?.dataset.act;
    if (!act || this.busy) return;
    if (act === "later") this.close(true);
    else if (act === "close") this.close(false);
    else if (act === "back") this.renderChoose();
    else if (act === "review") void this.review();
    else if (act === "apply") void this.apply();
  }

  // -- step 2: review ------------------------------------------------------

  private selection(id: string): Sel {
    const t = this.status!.targets.find((x) => x.target.id === id)!;
    const s = { ...this.sel.get(id)! };
    // Components a target doesn't have can't be asked for.
    if (t.target.kind === "windows") s.probe = false;
    return s;
  }

  private async review(): Promise<void> {
    this.busy = true;
    this.body.innerHTML = `<h2>Review changes</h2><p class="muted">Working out what would change…</p>`;
    try {
      this.plans.clear();
      for (const id of this.include) {
        this.plans.set(id, await this.invoke!<Plan>("setup_plan", { target: id, selection: this.selection(id) }));
      }
    } catch (e) {
      this.body.innerHTML = `<h2>Review changes</h2><p class="error">${esc(String(e))}</p><div class="actions"><button type="button" data-act="back">Back</button></div>`;
      return;
    } finally {
      this.busy = false;
    }
    const label = (id: string) => this.status!.targets.find((t) => t.target.id === id)!.target.label;
    const sections = [...this.plans.values()]
      .map((p) => {
        if (p.error) return `<section class="su-target"><div class="su-head"><b>${esc(label(p.target))}</b></div><p class="error">${esc(p.error)}</p><p class="muted small">Nothing will be changed here.</p></section>`;
        if (p.nothing) return `<section class="su-target"><div class="su-head"><b>${esc(label(p.target))}</b></div><p class="muted">Already as selected. Nothing to change.</p></section>`;
        const files = p.files.filter((f) => f.action !== "same");
        return `<section class="su-target">
          <div class="su-head"><b>${esc(label(p.target))}</b></div>
          ${files.length ? `<div class="k">Files</div><ul class="su-files mono">${files.map((f) => `<li><span class="su-act ${f.action}">${f.action}</span> ${esc(f.path)}</li>`).join("")}</ul>` : ""}
          ${p.diff ? `<div class="k">${esc(p.settings_path)}</div><pre class="su-diff">${diffHtml(p.diff)}</pre>` : ""}
          ${p.diff ? `<p class="muted small">Colony entries you registered by hand (a command running colony-hook or colony-approve.sh) are adopted: their command is replaced, so any extra arguments are dropped. Your other hooks are not touched.</p>` : ""}
          ${p.backup_path ? `<p class="muted small">Backup first: <span class="mono">${esc(p.backup_path)}</span></p>` : ""}
        </section>`;
      })
      .join("");
    const doable = [...this.plans.values()].some((p) => !p.error && !p.nothing);
    this.body.innerHTML = `
      <h2>Review changes</h2>
      ${[...this.plans.values()].some((p) => p.error) ? `<p class="error">Some targets can't be set up; the reason is under each one. ${doable ? "Apply will only change the others." : "Nothing can be applied."}</p>` : ""}
      ${sections || `<p class="muted">No targets selected.</p>`}
      <div class="actions">
        <button type="button" class="primary" data-act="apply" ${doable ? "" : "disabled"}>Apply these changes</button>
        <button type="button" data-act="back">Back</button>
      </div>`;
  }

  // -- step 3: result ------------------------------------------------------

  private async apply(): Promise<void> {
    this.busy = true;
    this.body.innerHTML = `<h2>Applying…</h2><p class="muted">Writing files. WSL distros can take a few seconds.</p>`;
    const results: Applied[] = [];
    for (const p of this.plans.values()) {
      if (p.nothing && !p.error) continue;
      // A target whose plan failed is part of the result, not silently dropped.
      if (p.error) {
        results.push({ target: p.target, ok: false, steps: [{ what: "Not changed", ok: false, detail: p.error }] });
        continue;
      }
      try {
        results.push(await this.invoke!<Applied>("setup_apply", { target: p.target, selection: this.selection(p.target), token: p.token }));
      } catch (e) {
        results.push({ target: p.target, ok: false, steps: [{ what: "Apply", ok: false, detail: String(e) }] });
      }
    }
    this.busy = false;
    await this.refresh();
    const label = (id: string) => this.status?.targets.find((t) => t.target.id === id)?.target.label ?? id;
    const allOk = results.every((r) => r.ok);
    // Only a fully successful run counts as done for this version.
    if (allOk) store(DISMISSED_KEY, this.status?.fingerprint ?? "");
    this.body.innerHTML = `
      <h2>${allOk ? "Done" : "Finished with errors"}</h2>
      ${results
        .map(
          (r) => `<section class="su-target"><div class="su-head"><b>${esc(label(r.target))}</b> <span class="su-state ${r.ok ? "current" : "missing"}">${r.ok ? "ok" : "failed"}</span></div>
          <ul class="su-steps">${r.steps.map((s) => `<li class="${s.ok ? "ok" : "bad"}">${s.ok ? "✓" : "✗"} ${esc(s.what)}${s.detail ? `<div class="error small">${esc(s.detail)}</div>` : ""}</li>`).join("")}</ul></section>`,
        )
        .join("")}
      <p class="muted small">Sessions that are already running pick up hooks the next time Claude Code starts. WSL probes attach within about 15 seconds.</p>
      <div class="actions"><button type="button" class="primary" data-act="close">Close</button>${allOk ? "" : `<button type="button" data-act="back">Back</button>`}</div>`;
  }
}

/** A unified diff with added and removed lines coloured. */
function diffHtml(diff: string): string {
  return diff
    .split("\n")
    .map((l) => {
      const cls = l.startsWith("+++") || l.startsWith("---") ? "hdr" : l.startsWith("+") ? "add" : l.startsWith("-") ? "del" : l.startsWith("@@") ? "hunk" : "";
      return `<span class="${cls}">${esc(l)}</span>`;
    })
    .join("\n");
}

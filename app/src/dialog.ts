// New session dialog: pick a project folder and a host, optionally a name and
// a first message, and Colony starts `claude` in a terminal it owns. The same
// dialog resumes an existing session.

import type { Daemon } from "./daemon";
import type { Agent } from "./types";

export interface Prefill {
  dir?: string;
  host?: string;
  /** Session id to resume instead of starting a new conversation. */
  resume?: string;
  resumeLabel?: string;
}

interface Folder {
  dir: string;
  label: string;
  host: string;
}

const OTHER = "__other__";

function hostLabel(host: string): string {
  return host === "win" ? "Windows" : host.replace("wsl:", "WSL · ");
}

export class NewSessionDialog {
  private dlg = document.getElementById("new-session") as HTMLDialogElement;
  private form = document.getElementById("ns-form") as HTMLFormElement;
  private title = document.getElementById("ns-title")!;
  private folder = document.getElementById("ns-folder") as HTMLSelectElement;
  private other = document.getElementById("ns-folder-other") as HTMLInputElement;
  private host = document.getElementById("ns-host") as HTMLSelectElement;
  private name = document.getElementById("ns-name") as HTMLInputElement;
  private mode = document.getElementById("ns-mode") as HTMLSelectElement;
  private chrome = document.getElementById("ns-chrome") as HTMLInputElement;
  private prompt = document.getElementById("ns-prompt") as HTMLTextAreaElement;
  private error = document.getElementById("ns-error")!;
  private start = document.getElementById("ns-start") as HTMLButtonElement;
  private resume: string | null = null;
  private folders: Folder[] = [];
  private prefill: Prefill = {};

  constructor(
    private daemon: Daemon,
    private onStarted: (started: { term: string; session_id: string }) => void,
    private termSize: () => { cols: number; rows: number },
  ) {
    this.folder.addEventListener("change", () => this.folderChanged());
    document.getElementById("ns-cancel")!.addEventListener("click", () => this.dlg.close());
    this.form.addEventListener("submit", (e) => {
      e.preventDefault();
      void this.submit();
    });
    // Opened before the daemon sent its first snapshot: fill in once it does.
    daemon.onChange(() => {
      if (this.dlg.open && (this.folders.length === 0 || this.host.options.length === 0)) this.populate();
    });
    this.prompt.addEventListener("keydown", (e) => {
      if (e.key === "Enter" && (e.ctrlKey || e.metaKey)) {
        e.preventDefault();
        void this.submit();
      }
    });
  }

  open(prefill: Prefill = {}): void {
    this.prefill = prefill;
    this.resume = prefill.resume ?? null;
    this.title.textContent = this.resume ? `Resume ${prefill.resumeLabel ?? "session"} in Colony` : "New session";
    this.start.textContent = this.resume ? "Resume" : "Start";
    this.name.closest("label")!.hidden = !!this.resume;
    this.error.textContent = "";
    this.name.value = "";
    this.prompt.value = "";
    this.populate();
    this.dlg.showModal();
    (this.resume ? this.prompt : this.folder).focus();
  }

  /** Folder and host choices from what the daemon currently knows. */
  private populate(): void {
    const prefill = this.prefill;

    // Known project folders, most recently active first.
    const seen = new Map<string, Folder & { at: number }>();
    for (const a of this.daemon.agents.values()) {
      const dir = a.project_dir ?? a.cwd;
      if (a.kind !== "main" || !dir || !a.project_key) continue;
      const prev = seen.get(a.project_key);
      if (!prev || prev.at < a.last_event_at) {
        seen.set(a.project_key, { dir, label: `${a.project_name ?? dir} — ${dir}`, host: a.host, at: a.last_event_at });
      }
    }
    this.folders = [...seen.values()].sort((x, y) => y.at - x.at);
    if (prefill.dir && !this.folders.some((f) => f.dir === prefill.dir)) {
      this.folders.unshift({ dir: prefill.dir, label: prefill.dir, host: prefill.host ?? "win" });
    }
    this.folder.innerHTML = "";
    for (const f of this.folders) this.folder.add(new Option(f.label, f.dir));
    this.folder.add(new Option("Other folder…", OTHER));
    this.folder.value = prefill.dir ?? this.folders[0]?.dir ?? OTHER;
    this.folder.disabled = !!this.resume;

    this.host.innerHTML = "";
    for (const h of this.daemon.hosts) {
      const o = new Option(h.available ? h.label : `${h.label} (${h.note ?? "unavailable"})`, h.id);
      o.disabled = !h.available;
      this.host.add(o);
    }
    this.folderChanged(prefill.host);
    this.host.disabled = !!this.resume;
  }

  /** Default the host to the one this folder's sessions use. */
  private folderChanged(preferred?: string): void {
    const isOther = this.folder.value === OTHER;
    this.other.hidden = !isOther;
    this.other.required = isOther;
    if (isOther) this.other.focus();
    const want = preferred ?? this.folders.find((f) => f.dir === this.folder.value)?.host;
    const usable = (id?: string) => this.daemon.hosts.some((h) => h.id === id && h.available);
    if (usable(want)) this.host.value = want!;
    else this.host.value = this.daemon.hosts.find((h) => h.available)?.id ?? "";
  }

  private async submit(): Promise<void> {
    const dir = this.folder.value === OTHER ? this.other.value.trim() : this.folder.value;
    if (!dir) {
      this.error.textContent = "Choose a folder to start in.";
      return;
    }
    if (!this.host.value) {
      this.error.textContent = "No host can run Claude Code. Install Claude Code on Windows or in a WSL distro.";
      return;
    }
    this.start.disabled = true;
    this.error.textContent = "";
    try {
      const started = await this.daemon.spawn({
        host: this.host.value,
        dir,
        prompt: this.prompt.value.trim() || undefined,
        name: this.resume ? undefined : this.name.value.trim() || undefined,
        resume: this.resume ?? undefined,
        permission_mode: this.mode.value || undefined,
        // Off unless ticked, which also skips Claude in Chrome's first-run question.
        chrome: this.chrome.checked,
        ...this.termSize(),
      });
      this.dlg.close();
      this.onStarted(started);
    } catch (e) {
      this.error.textContent = e instanceof Error ? e.message : String(e);
    } finally {
      this.start.disabled = false;
    }
  }
}

/** Why resuming this session here would make a second copy, if it would. */
export function resumeWarning(a: Agent): string | null {
  // Running copies the session registry knows about, whatever the bot's state.
  const pids = a.pids?.length ? a.pids : [];
  if (!pids.length) return null;
  const which = pids.length === 1 ? `process ${pids[0]}` : `processes ${pids.join(", ")}`;
  if (a.entrypoint === "claude-desktop") {
    return `It's still running in the Claude desktop app (${which}). The app keeps sessions running in the background after you close their window.`;
  }
  return `It's still running in a terminal on ${hostLabel(a.host)} (${which}).`;
}

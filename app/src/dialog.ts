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
  private prompt = document.getElementById("ns-prompt") as HTMLTextAreaElement;
  private error = document.getElementById("ns-error")!;
  private start = document.getElementById("ns-start") as HTMLButtonElement;
  private resume: string | null = null;
  private folders: Folder[] = [];
  private prefill: Prefill = {};

  constructor(
    private daemon: Daemon,
    private onStarted: (sessionId: string) => void,
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
      const { session_id } = await this.daemon.spawn({
        host: this.host.value,
        dir,
        prompt: this.prompt.value.trim() || undefined,
        name: this.resume ? undefined : this.name.value.trim() || undefined,
        resume: this.resume ?? undefined,
        ...this.termSize(),
      });
      this.dlg.close();
      this.onStarted(session_id);
    } catch (e) {
      this.error.textContent = e instanceof Error ? e.message : String(e);
    } finally {
      this.start.disabled = false;
    }
  }
}

export function resumeWarning(a: Agent): string | null {
  const live = a.state !== "ended" && a.state !== "crashed" && a.pid !== null;
  if (!live) return null;
  const where = a.entrypoint === "claude-desktop" ? "the Claude desktop app" : `a terminal on ${hostLabel(a.host)}`;
  return `This session is still open in ${where}. Close it there first, or both copies will write to the same conversation.`;
}

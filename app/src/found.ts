// Sessions from before: after a crash, reboot or closing Colony, colonyd finds
// sessions in Colony worktrees it had lost track of and offers them here. The
// ones picked come back on the map as paused bots, with Resume ready; the rest
// aren't offered again. "Not now" leaves the offer in the top bar.

import type { Daemon } from "./daemon";
import type { Found } from "./types";

const esc = (s: string) => s.replace(/[&<>"']/g, (c) => `&#${c.charCodeAt(0)};`);

function hostLabel(host: string): string {
  return host === "win" ? "Windows" : host.replace("wsl:", "WSL · ");
}

const lastPart = (p: string) => p.replace(/[\\/]+$/, "").split(/[\\/]/).pop() ?? p;

/** "3 days ago", roughly. */
export function agoText(ms: number): string {
  const m = Math.round(Math.max(0, ms) / 60_000);
  if (m < 1) return "just now";
  if (m < 60) return `${m} min ago`;
  const h = Math.round(m / 60);
  if (h < 48) return `${h} h ago`;
  return `${Math.round(h / 24)} days ago`;
}

export class FoundDialog {
  private dlg = document.getElementById("found") as HTMLDialogElement;
  private form = document.getElementById("found-form") as HTMLFormElement;
  private list = document.getElementById("found-list")!;
  private go = document.getElementById("found-go") as HTMLButtonElement;
  /** The offer last shown, so the same one doesn't pop up again on every change. */
  private shown = "";

  constructor(private daemon: Daemon) {
    daemon.onChange(() => this.sync());
    this.form.addEventListener("change", () => this.count());
    this.form.addEventListener("submit", (e) => {
      e.preventDefault();
      this.answer(this.picked());
    });
    document.getElementById("found-later")!.addEventListener("click", () => this.dlg.close());
    document.getElementById("found-none")!.addEventListener("click", () => this.answer([]));
    document.addEventListener("click", (e) => {
      if ((e.target as HTMLElement).closest("[data-show-found]")) this.open();
    });
  }

  private sync(): void {
    const items = this.daemon.found;
    if (!items.length) {
      this.shown = "";
      if (this.dlg.open) this.dlg.close();
      return;
    }
    const key = items.map((f) => f.session_id).join(",");
    if (key !== this.shown && !this.dlg.open && !document.querySelector("dialog[open]")) {
      this.shown = key;
      this.open();
    }
  }

  open(): void {
    const items = this.daemon.found;
    if (!items.length) return;
    const now = this.daemon.now();
    this.list.innerHTML = items.map((f) => this.row(f, now)).join("");
    this.count();
    if (!this.dlg.open) this.dlg.showModal();
  }

  private row(f: Found, now: number): string {
    const wt = lastPart(f.worktree);
    const title = f.name && f.name !== wt ? `${esc(f.name)} <span class="muted">· ${esc(wt)}</span>` : esc(wt);
    return `<label class="check found-row">
      <input type="checkbox" value="${esc(f.session_id)}" checked />
      <span>
        <b>${title}</b>
        <span class="muted small">${esc(lastPart(f.repo))} · ${esc(hostLabel(f.host))} · last active ${esc(agoText(now - f.last_at))}</span>
        <span class="mono small">${esc(f.dir)}</span>
      </span>
    </label>`;
  }

  private picked(): string[] {
    return [...this.list.querySelectorAll<HTMLInputElement>("input:checked")].map((i) => i.value);
  }

  private count(): void {
    const n = this.picked().length;
    this.go.disabled = n === 0;
    this.go.textContent = n ? `Bring back ${n}` : "Bring back";
  }

  private answer(pick: string[]): void {
    if (this.daemon.bringBack(pick)) this.dlg.close();
  }
}

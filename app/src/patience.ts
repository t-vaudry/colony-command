// Patience: an OS notification when a bot has waited on you past its budget.
// The decision is colony-core's `patience` module, run by the desktop shell
// (`patience_tick`); this feeds it the waiting bots about every 10 s, keeps
// the settings, and brings the bot into view when you come back to the window.

import type { Daemon } from "./daemon";
import { severity, type Agent } from "./types";

type Invoke = <T>(cmd: string, args?: Record<string, unknown>) => Promise<T>;

/** Minutes each kind of wait may last; 0 turns that kind off. */
export interface PatienceSettings {
  enabled: boolean;
  permission_min: number;
  question_min: number;
  review_min: number;
}

export const DEFAULTS: PatienceSettings = { enabled: true, permission_min: 5, question_min: 15, review_min: 60 };

const KEY = "colony.patience";
const TICK_MS = 10_000;
/** Coming back to the window this soon after a notification selects its bot. */
const RETURN_WINDOW_MS = 10 * 60_000;

interface Notice {
  title: string;
  body: string;
  agent: string | null;
}

function invoker(): Invoke | null {
  const t = (window as unknown as { __TAURI__?: { core?: { invoke: Invoke } } }).__TAURI__;
  return t?.core?.invoke ?? null;
}

/** A budget as the settings allow it: whole minutes, 0 to 10080 (a week). */
export function clampMinutes(v: unknown, fallback: number): number {
  const n = typeof v === "number" ? v : Number(v);
  return Number.isFinite(n) ? Math.min(10_080, Math.max(0, Math.round(n))) : fallback;
}

export function loadSettings(): PatienceSettings {
  try {
    const o = JSON.parse(localStorage.getItem(KEY) ?? "{}") as Partial<PatienceSettings>;
    return {
      enabled: o.enabled !== false,
      permission_min: clampMinutes(o.permission_min, DEFAULTS.permission_min),
      question_min: clampMinutes(o.question_min, DEFAULTS.question_min),
      review_min: clampMinutes(o.review_min, DEFAULTS.review_min),
    };
  } catch {
    return { ...DEFAULTS };
  }
}

function saveSettings(s: PatienceSettings): void {
  try {
    localStorage.setItem(KEY, JSON.stringify(s));
  } catch {
    // Private mode: the settings last until the window closes.
  }
}

export class PatienceNotifier {
  private invoke = invoker();
  settings = loadSettings();
  private last: { agent: string; at: number } | null = null;
  private dlg = document.getElementById("patience") as HTMLDialogElement;
  private btn = document.getElementById("patience-btn") as HTMLButtonElement;

  constructor(
    private daemon: Daemon,
    private select: (id: string) => void,
  ) {
    // Notifications are the desktop app's; in a browser there is nothing to show them with.
    if (!this.invoke) {
      this.btn.hidden = true;
      return;
    }
    this.btn.addEventListener("click", () => this.open());
    this.dlg.addEventListener("close", () => this.save());
    document.getElementById("patience-cancel")!.addEventListener("click", () => this.dlg.close("cancel"));
    document.getElementById("patience-test")!.addEventListener("click", () => void this.invoke!("patience_test").catch(() => {}));
    window.addEventListener("focus", () => this.comeBack());
    setInterval(() => void this.tick(), TICK_MS);
  }

  /** Bots waiting on a human, main agents only (a subagent's wait is its parent's). */
  private waiting(): Agent[] {
    return [...this.daemon.agents.values()].filter((a) => a.kind === "main" && severity(a.state) !== null);
  }

  private async tick(): Promise<void> {
    if (this.daemon.status !== "live") return;
    const items = this.waiting().map((a) => ({
      id: a.id,
      name: a.name,
      project: a.project_name,
      state: a.state,
      state_since: a.state_since,
    }));
    const attended = document.hasFocus() && !document.hidden;
    try {
      const shown = await this.invoke!<Notice[]>("patience_tick", { now: this.daemon.now(), items, settings: this.settings, attended });
      // A desktop toast can't tell us it was clicked, so remember the bot that has
      // waited longest (shown longest first) and select it when the window next gets focus.
      if (shown.length) {
        const one = shown.find((n) => n.agent);
        this.last = one?.agent ? { agent: one.agent, at: Date.now() } : null;
      }
    } catch {
      // The shell is older than this build, or notifications are refused: stay quiet.
    }
  }

  private comeBack(): void {
    const l = this.last;
    this.last = null;
    if (l && Date.now() - l.at < RETURN_WINDOW_MS && this.daemon.agents.has(l.agent)) this.select(l.agent);
  }

  private input(name: string): HTMLInputElement {
    return this.dlg.querySelector(`[name="${name}"]`) as HTMLInputElement;
  }

  private open(): void {
    const s = this.settings;
    this.input("enabled").checked = s.enabled;
    this.input("permission_min").value = String(s.permission_min);
    this.input("question_min").value = String(s.question_min);
    this.input("review_min").value = String(s.review_min);
    this.dlg.showModal();
  }

  private save(): void {
    if (this.dlg.returnValue !== "save") return;
    this.settings = {
      enabled: this.input("enabled").checked,
      permission_min: clampMinutes(this.input("permission_min").value, DEFAULTS.permission_min),
      question_min: clampMinutes(this.input("question_min").value, DEFAULTS.question_min),
      review_min: clampMinutes(this.input("review_min").value, DEFAULTS.review_min),
    };
    saveSettings(this.settings);
  }
}

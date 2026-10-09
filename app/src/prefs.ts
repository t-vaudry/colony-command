/** Attention-budget preferences, kept in localStorage (per machine, not synced). */

/** "system" follows the OS setting; the others override it inside the app. */
export type MotionPref = "system" | "reduce" | "full";

const KEY_FOCUS = "colony.focus";
const KEY_MOTION = "colony.motion";
const MOTION_ORDER: MotionPref[] = ["system", "reduce", "full"];

const media = matchMedia("(prefers-reduced-motion: reduce)");

function read(key: string): string | null {
  try {
    return localStorage.getItem(key);
  } catch {
    return null;
  }
}

function write(key: string, value: string): void {
  try {
    localStorage.setItem(key, value);
  } catch {
    // Storage blocked: the choice just lasts until the window closes.
  }
}

class Prefs {
  focus = read(KEY_FOCUS) === "1";
  motion: MotionPref = MOTION_ORDER.find((m) => m === read(KEY_MOTION)) ?? "system";
  private listeners: Array<() => void> = [];

  constructor() {
    media.addEventListener("change", () => this.changed());
    this.apply();
  }

  /** True when motion should be cut back: the in-app override, else the OS. */
  get reduced(): boolean {
    return this.motion === "system" ? media.matches : this.motion === "reduce";
  }

  onChange(fn: () => void): void {
    this.listeners.push(fn);
  }

  setFocus(on: boolean): void {
    this.focus = on;
    write(KEY_FOCUS, on ? "1" : "0");
    this.changed();
  }

  toggleFocus(): void {
    this.setFocus(!this.focus);
  }

  /** system -> reduce -> full -> system. */
  cycleMotion(): void {
    this.motion = MOTION_ORDER[(MOTION_ORDER.indexOf(this.motion) + 1) % MOTION_ORDER.length];
    write(KEY_MOTION, this.motion);
    this.changed();
  }

  private changed(): void {
    this.apply();
    for (const fn of this.listeners) fn();
  }

  /** Mirror onto <html> so CSS can follow (data-motion, data-focus). */
  private apply(): void {
    const root = document.documentElement;
    root.dataset.motion = this.reduced ? "reduce" : "full";
    root.dataset.focus = this.focus ? "on" : "off";
  }
}

export const prefs = new Prefs();

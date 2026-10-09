// The terminal pane under the map: a live view of one Colony-owned session,
// with full keyboard input, for anything the reply box can't do (answering a
// trust prompt, picking a menu option, slash commands).

import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import "@xterm/xterm/css/xterm.css";
import type { Daemon } from "./daemon";

function cssVar(name: string): string {
  return getComputedStyle(document.documentElement).getPropertyValue(name).trim();
}

export class TerminalPane {
  /** Terminal id currently shown, if any. */
  attached: string | null = null;
  private term: Terminal;
  private fit = new FitAddon();
  private pane = document.getElementById("term-pane")!;
  private title = document.getElementById("term-title")!;
  private host = document.getElementById("term-host")!;
  private opened = false;

  constructor(
    private daemon: Daemon,
    private onLayout: () => void,
  ) {
    this.term = new Terminal({
      fontFamily: '"JetBrains Mono", "Cascadia Mono", Consolas, monospace',
      fontSize: 12.5,
      cursorBlink: true,
      scrollback: 5000,
      theme: this.theme(),
    });
    this.term.loadAddon(this.fit);
    this.term.onData((data) => {
      if (this.attached) this.daemon.input(this.attached, data);
    });
    this.term.onResize(({ cols, rows }) => {
      if (this.attached) this.daemon.resize(this.attached, cols, rows);
    });
    daemon.onTermData((term, bytes, reset) => {
      if (term !== this.attached) return;
      if (reset) this.term.reset();
      this.term.write(bytes);
    });
    new ResizeObserver(() => this.refit()).observe(this.host);
    document.getElementById("term-close")!.addEventListener("click", () => this.hide());
    matchMedia("(prefers-color-scheme: dark)").addEventListener("change", () => {
      this.term.options.theme = this.theme();
    });
  }

  private theme() {
    return {
      background: cssVar("--term-bg"),
      foreground: cssVar("--term-fg"),
      cursor: cssVar("--accent"),
      selectionBackground: cssVar("--term-sel"),
    };
  }

  /** Columns and rows the pane will have, for starting a session at the right size. */
  size(): { cols: number; rows: number } {
    return { cols: Math.max(60, this.term.cols || 120), rows: Math.max(16, this.term.rows || 30) };
  }

  get visible(): boolean {
    return !this.pane.hidden;
  }

  show(term: string, label: string): void {
    this.title.textContent = label;
    const wasHidden = this.pane.hidden;
    this.pane.hidden = false;
    if (!this.opened) {
      this.term.open(this.host);
      this.opened = true;
    }
    if (wasHidden) this.onLayout();
    if (this.attached !== term) {
      this.attached = term;
      this.term.reset();
      // Fit to the pane before the replay arrives, so the scrollback is
      // written at the final size instead of being reflowed (garbled) after.
      this.refit();
      this.daemon.attach(term);
    } else {
      this.refit();
    }
    this.term.focus();
  }

  hide(): void {
    if (this.pane.hidden) return;
    this.pane.hidden = true;
    if (this.attached) this.daemon.detach();
    this.attached = null;
    this.onLayout();
  }

  /** The daemon reconnected: the server forgot what this map was watching. */
  reattach(): void {
    if (this.attached) this.daemon.attach(this.attached);
  }

  private refit(): void {
    if (!this.opened || this.pane.hidden) return;
    try {
      this.fit.fit();
    } catch {
      // Not laid out yet; the ResizeObserver will try again.
    }
  }
}

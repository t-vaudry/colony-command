// The terminal pane under the map: a live view of one Colony-owned session,
// with full keyboard input, for anything the reply box can't do (answering a
// trust prompt, picking a menu option, slash commands).

import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { WebLinksAddon } from "@xterm/addon-web-links";
import "@xterm/xterm/css/xterm.css";
import type { Daemon } from "./daemon";
import { platform } from "./platform";
import { ctrlify, keyData } from "./termkeys";

/** Hand a link to the default browser. xterm's own handler would confirm, then open a blank window. */
export function openLink(uri: string): void {
  platform().openLink(uri);
}

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
  private settleGen = 0;
  private keys = document.getElementById("term-keys");
  /** Ctrl is armed from the key bar: the next typed character becomes a control code. */
  private ctrl = false;

  constructor(
    private daemon: Daemon,
    private onLayout: () => void,
  ) {
    this.term = new Terminal({
      fontFamily: '"JetBrains Mono", "Cascadia Mono", Consolas, monospace',
      fontSize: 12.5,
      cursorBlink: true,
      scrollback: 5000,
      linkHandler: { activate: (_e, uri) => openLink(uri) },
      theme: this.theme(),
    });
    this.term.loadAddon(this.fit);
    this.term.loadAddon(new WebLinksAddon((_e, uri) => openLink(uri)));
    this.term.onData((data) => this.type(data));
    this.term.onResize(({ cols, rows }) => {
      // On a paired device the PC owns the size: a resize from here would be ignored.
      if (this.attached && !this.daemon.remote) this.daemon.resize(this.attached, cols, rows);
    });
    daemon.onTermData((term, bytes, reset, size) => {
      if (term !== this.attached) return;
      if (reset) this.term.reset();
      // Draw at the session's own size, so its layout is right; the pane scrolls if it is wider.
      this.sizeFromHost = this.daemon.remote && !!size;
      if (this.daemon.remote && size) this.term.resize(size.cols, size.rows);
      this.term.write(bytes);
    });
    this.bindKeys();
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

  /** Send typed or key-bar input, unless this device may only watch. */
  private type(data: string): void {
    if (!this.attached || this.daemon.scope === "view") return;
    if (this.ctrl) {
      data = ctrlify(data);
      this.setCtrl(false);
    }
    this.daemon.input(this.attached, data);
  }

  private setCtrl(on: boolean): void {
    this.ctrl = on;
    const b = document.getElementById("term-ctrl");
    b?.setAttribute("aria-pressed", String(on));
    b?.classList.toggle("armed", on);
  }

  /** The key bar and interrupt button, for a touchscreen or a paired device. */
  private bindKeys(): void {
    this.keys?.addEventListener("click", (e) => {
      const b = (e.target as HTMLElement).closest("button");
      if (!b || !this.attached) return;
      if (b.id === "term-ctrl") this.setCtrl(!this.ctrl);
      else if (b.id === "term-interrupt") this.daemon.interrupt(this.attached);
      else {
        const data = keyData(b.dataset.key ?? "");
        if (data !== null) this.type(data);
      }
      // A tap on a button must not take the focus from the terminal, or the soft keyboard drops.
      this.term.focus();
    });
    this.keys?.addEventListener("pointerdown", (e) => e.preventDefault());
  }

  /** Show the key bar when there is no physical keyboard to assume, and not at all when view-only. */
  private syncKeys(): void {
    if (!this.keys) return;
    const touch = this.daemon.remote || matchMedia("(pointer: coarse)").matches;
    this.keys.hidden = !touch || this.daemon.scope === "view";
  }

  /** Columns and rows the pane will have, for starting a session at the right size. */
  size(): { cols: number; rows: number } {
    return { cols: Math.max(60, this.term.cols || 120), rows: Math.max(16, this.term.rows || 30) };
  }

  /** A paired device draws at the session's own size and does not fit the pane. */
  private sizeFromHost = false;

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
    const switched = this.attached !== term;
    if (switched) {
      this.attached = term;
      this.term.reset();
      // Fit to the pane before the replay arrives, so the scrollback is
      // written at the final size instead of being reflowed (garbled) after.
      this.refit();
      this.daemon.attach(term);
      this.syncSize(term);
    } else {
      this.refit();
    }
    this.syncKeys();
    this.term.focus();
    this.settle(term, switched);
  }

  /**
   * Tell the session its real size. xterm only reports a change, so after a
   * reset or a re-show the PTY can still be at whatever the last viewer used
   * while this pane thinks nothing changed; that mismatch is the garbled view.
   */
  private syncSize(term: string): void {
    if (this.daemon.remote) return;
    if (this.term.cols > 0 && this.term.rows > 0) this.daemon.resize(term, this.term.cols, this.term.rows);
  }

  /**
   * Once the pane has really been laid out (the show above races the layout
   * and the first font measurement), fit again, repaint, and make the session
   * redraw itself at the final size by briefly nudging it one column narrower.
   */
  private settle(term: string, nudge = true): void {
    const gen = ++this.settleGen;
    const run = (delay: number, nudge: boolean) =>
      setTimeout(() => {
        if (gen !== this.settleGen || this.attached !== term || this.pane.hidden) return;
        this.refit();
        this.term.refresh(0, this.term.rows - 1);
        if (!nudge) return;
        const { cols, rows } = this.term;
        if (cols > 1 && !this.daemon.remote) this.daemon.resize(term, cols - 1, rows);
        this.syncSize(term);
      }, delay);
    requestAnimationFrame(() => requestAnimationFrame(() => run(0, false)));
    if (nudge) run(250, true);
  }

  hide(): void {
    if (this.pane.hidden) return;
    this.pane.hidden = true;
    this.settleGen++;
    if (this.attached) this.daemon.detach();
    this.attached = null;
    this.onLayout();
  }

  /** The daemon reconnected: the server forgot what this map was watching. */
  reattach(): void {
    if (!this.attached) return;
    this.daemon.attach(this.attached);
    this.syncSize(this.attached);
    this.settle(this.attached);
  }

  private refit(): void {
    if (!this.opened || this.pane.hidden || this.sizeFromHost) return;
    try {
      this.fit.fit();
    } catch {
      // Not laid out yet; the ResizeObserver will try again.
    }
  }
}

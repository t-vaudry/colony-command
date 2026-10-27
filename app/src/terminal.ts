// The terminal pane under the map: a live view of one Colony-owned session,
// with full keyboard input, for anything the reply box can't do (answering a
// trust prompt, picking a menu option, slash commands).

import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { WebLinksAddon } from "@xterm/addon-web-links";
import "@xterm/xterm/css/xterm.css";
import type { Daemon } from "./daemon";

/** Hand a link to the default browser. xterm's own handler would confirm, then open a blank window. */
export function openLink(uri: string): void {
  const invoke = (window as unknown as { __TAURI__?: { core?: { invoke: (cmd: string, args: object) => Promise<void> } } }).__TAURI__?.core?.invoke;
  if (invoke) void invoke("open_link", { url: uri });
  else window.open(uri, "_blank", "noopener");
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
    this.term.focus();
    this.settle(term, switched);
  }

  /**
   * Tell the session its real size. xterm only reports a change, so after a
   * reset or a re-show the PTY can still be at whatever the last viewer used
   * while this pane thinks nothing changed; that mismatch is the garbled view.
   */
  private syncSize(term: string): void {
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
        if (cols > 1) this.daemon.resize(term, cols - 1, rows);
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
    if (!this.opened || this.pane.hidden) return;
    try {
      this.fit.fit();
    } catch {
      // Not laid out yet; the ResizeObserver will try again.
    }
  }
}

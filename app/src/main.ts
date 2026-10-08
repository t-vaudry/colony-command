import "./style.css";
import { Daemon } from "./daemon";
import { NewSessionDialog, type Prefill } from "./dialog";
import { Hud } from "./hud";
import { TerminalPane } from "./terminal";
import type { Agent } from "./types";
import { World } from "./world";

const daemon = new Daemon();

function select(id: string | null): void {
  world.selected = id;
  hud.selected = id;
  hud.schedule();
  // Follow the selection with the terminal pane when it's open.
  const a = id ? daemon.agents.get(id) : undefined;
  if (terminal.visible && a?.terminal) showTerminal(a);
}

function showTerminal(a: Agent): void {
  if (a.terminal) terminal.show(a.terminal, `${a.name} · ${a.project_name ?? ""}`);
}

function newSession(prefill?: Prefill): void {
  dialog.open(prefill);
}

const world = new World(daemon, select);
const terminal = new TerminalPane(daemon, () => requestAnimationFrame(() => world.resize()));
const hud = new Hud(daemon, { select, showTerminal, newSession });
const dialog = new NewSessionDialog(
  daemon,
  (sessionId) => {
    // The new bot appears with the TerminalAttached event; open its terminal.
    const open = () => {
      const a = daemon.agents.get(sessionId);
      if (!a) return false;
      select(sessionId);
      showTerminal(a);
      return true;
    };
    if (!open()) {
      const timer = setInterval(() => open() && clearInterval(timer), 100);
      setTimeout(() => clearInterval(timer), 5000);
    }
  },
  () => terminal.size(),
);

document.getElementById("new-session-btn")!.addEventListener("click", () => {
  const a = world.selected ? daemon.agents.get(world.selected) : undefined;
  newSession(a ? { dir: a.project_dir ?? a.cwd ?? undefined, host: a.host } : undefined);
});

// After a daemon restart, re-subscribe the terminal pane.
let wasLive = false;
daemon.onChange(() => {
  if (daemon.status === "live" && !wasLive) terminal.reattach();
  wasLive = daemon.status === "live";
});

window.addEventListener("keydown", (e) => {
  const target = e.target as HTMLElement;
  if (target.closest("input, textarea, select, dialog, .xterm")) return;
  if (e.code === "Space") {
    // Jump to the next agent that needs a human.
    e.preventDefault();
    const queue = daemon.porch();
    if (!queue.length) return;
    const i = queue.findIndex((a) => a.id === world.selected);
    select(queue[(i + 1) % queue.length].id);
  } else if (e.key === "0") {
    world.fit();
  } else if (e.key === "n" || e.key === "N") {
    e.preventDefault();
    document.getElementById("new-session-btn")!.click();
  } else if (e.key === "t" || e.key === "T") {
    const a = world.selected ? daemon.agents.get(world.selected) : undefined;
    if (terminal.visible) terminal.hide();
    else if (a?.terminal) showTerminal(a);
  } else if (e.key === "Escape") {
    select(null);
  }
});

void world.init(document.getElementById("map")!).then(() => daemon.start());

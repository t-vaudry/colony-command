import "./style.css";
import { Daemon } from "./daemon";
import { NewSessionDialog, repoDir, type Prefill } from "./dialog";
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

/** Agent whose terminal the pane shows, so it can follow a replaced terminal. */
let paneAgent: string | null = null;

function showTerminal(a: Agent): void {
  if (!a.terminal) return;
  paneAgent = a.id;
  terminal.show(a.terminal, `${a.name} · ${a.project_name ?? ""}`);
}

function newSession(prefill?: Prefill): void {
  dialog.open(prefill);
}

const world = new World(daemon, select);
const terminal = new TerminalPane(daemon, () => requestAnimationFrame(() => world.resize()));
async function signIn(a: Agent): Promise<void> {
  const { term } = await daemon.signIn(a.id, terminal.size());
  // Not the bot's terminal: don't let the pane snap back to it.
  paneAgent = null;
  terminal.show(term, `Sign in to ${a.auth_need?.label ?? "the service"} · ${a.name}`);
}

const hud = new Hud(daemon, { select, showTerminal, newSession, signIn });
const dialog = new NewSessionDialog(
  daemon,
  ({ term, session_id }) => {
    // Use the terminal id from the spawn reply: when resuming, the bot
    // already exists and still carries its old (closed) terminal for a moment.
    paneAgent = session_id;
    const label = daemon.agents.get(session_id)?.name ?? "New session";
    terminal.show(term, label);
    select(session_id);
  },
  () => terminal.size(),
);

document.getElementById("new-session-btn")!.addEventListener("click", () => {
  const a = world.selected ? daemon.agents.get(world.selected) : undefined;
  newSession(a ? { dir: repoDir(a.project_dir ?? a.cwd) ?? undefined, host: a.host } : undefined);
});

// After a daemon restart, re-subscribe the terminal pane. And if the agent in
// the pane gets a new terminal (resumed again), follow it.
let wasLive = false;
daemon.onChange(() => {
  if (daemon.status === "live" && !wasLive) terminal.reattach();
  wasLive = daemon.status === "live";
  const a = paneAgent ? daemon.agents.get(paneAgent) : undefined;
  if (terminal.visible && a?.terminal && a.terminal !== terminal.attached) showTerminal(a);
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

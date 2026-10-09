import "./style.css";
import { Daemon } from "./daemon";
import { NewSessionDialog, repoDir, type Prefill } from "./dialog";
import { Hud } from "./hud";
import { prefs } from "./prefs";
import { initLayout } from "./layout";
import { TerminalPane } from "./terminal";
import type { Agent } from "./types";
import { PatienceNotifier } from "./patience";
import { SetupDialog } from "./setup";
import { initUpdateNotice } from "./update";
import { World } from "./world";

const daemon = new Daemon();

function select(id: string | null): void {
  world.selected = id;
  hud.selected = id;
  hud.schedule();
  // Follow the selection with the terminal pane when it's open.
  const a = id ? daemon.agents.get(id) : undefined;
  if (!id) {
    // Nothing selected: close the pane rather than leave the last bot's terminal open.
    paneAgent = null;
    terminal.hide();
  } else if (terminal.visible && a?.terminal) showTerminal(a);
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
async function fixNeed(a: Agent): Promise<void> {
  const { term } = await daemon.fixNeed(a.id, terminal.size());
  // Not the bot's terminal: don't let the pane snap back to it.
  paneAgent = null;
  const what = a.auth_need?.kind === "install" ? "Install" : "Sign in to";
  terminal.show(term, `${what} ${a.auth_need?.label ?? "the tool"} · ${a.name}`);
}

function started({ term, session_id }: { term: string; session_id: string }): void {
  // Use the terminal id from the spawn reply: when resuming, the bot
  // already exists and still carries its old (closed) terminal for a moment.
  paneAgent = session_id;
  const label = daemon.agents.get(session_id)?.name ?? "New session";
  terminal.show(term, label);
  select(session_id);
}

/** One-click "resume in Colony": same folder and host, no dialog. */
async function resumeNow(a: Agent, prompt?: string): Promise<void> {
  const dir = a.project_dir ?? a.cwd;
  if (!dir) throw new Error("Colony doesn't know this session's folder; use New session to resume it.");
  started(await daemon.spawn({ host: a.host, dir, resume: a.session_id, prompt, chrome: false, ...terminal.size() }));
}

const hud = new Hud(daemon, { select, showTerminal, newSession, fixNeed, resumeNow });
const dialog = new NewSessionDialog(daemon, started, () => terminal.size());

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
  } else if (e.key === "f" || e.key === "F") {
    if (e.ctrlKey || e.metaKey || e.altKey) return;
    prefs.toggleFocus();
  } else if (e.key === "Escape") {
    select(null);
  }
});

// Attention-budget controls in the top bar (Focus mode, motion override).
const focusBtn = document.getElementById("focus-btn")!;
const motionBtn = document.getElementById("motion-btn")!;
const MOTION_LABEL = { system: "Motion: system", reduce: "Motion: calm", full: "Motion: full" } as const;
function syncPrefs(): void {
  focusBtn.textContent = prefs.focus ? "Focus: on" : "Focus: off";
  focusBtn.setAttribute("aria-pressed", String(prefs.focus));
  focusBtn.classList.toggle("on", prefs.focus);
  focusBtn.title = "Focus mode: dim and calm everything that does not need you (F)";
  motionBtn.textContent = MOTION_LABEL[prefs.motion];
  motionBtn.title =
    "Motion: follow the system's reduced-motion setting, always calm (no bounces, particles or pulses), or full. Click to change.";
}
focusBtn.addEventListener("click", () => prefs.toggleFocus());
motionBtn.addEventListener("click", () => prefs.cycleMotion());
prefs.onChange(syncPrefs);
syncPrefs();

initLayout();
new SetupDialog();
initUpdateNotice();
new PatienceNotifier(daemon, select);
void world.init(document.getElementById("map")!).then(() => daemon.start());

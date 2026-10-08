import "./style.css";
import { Daemon } from "./daemon";
import { Hud } from "./hud";
import { World } from "./world";

const daemon = new Daemon();
let world: World;
let hud: Hud;

function select(id: string | null): void {
  world.selected = id;
  hud.selected = id;
  hud.schedule();
}

world = new World(daemon, select);
hud = new Hud(daemon, select);

window.addEventListener("keydown", (e) => {
  if ((e.target as HTMLElement).closest("input, textarea")) return;
  if (e.code === "Space") {
    // Jump to the next agent that needs a human.
    e.preventDefault();
    const queue = daemon.porch();
    if (!queue.length) return;
    const i = queue.findIndex((a) => a.id === world.selected);
    const next = queue[(i + 1) % queue.length];
    select(next.id);
  } else if (e.key === "0") {
    world.fit();
  } else if (e.key === "Escape") {
    select(null);
  }
});

void world.init(document.getElementById("map")!).then(() => daemon.start());

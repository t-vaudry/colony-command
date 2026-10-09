// The colony map: districts per project, the front porch, the review dock,
// and one bot per agent. Agent state comes from the daemon (truth layer);
// everything here is presentation, and must never contradict it.

import { Application, Container, Graphics, Text } from "pixi.js";
import type { Daemon } from "./daemon";
import { money, spentToday, type Agent, type AgentState } from "./types";

interface Palette {
  ground: number;
  grid: number;
  plot: number;
  plotEdge: number;
  porch: number;
  porchEdge: number;
  ink: number;
  muted: number;
  crit: number;
  input: number;
  ok: number;
  done: number;
  idle: number;
  accent: number;
  projects: number[];
}

function cssColor(name: string): number {
  const v = getComputedStyle(document.documentElement).getPropertyValue(name).trim();
  return Number.parseInt(v.replace("#", ""), 16) || 0;
}

function readPalette(): Palette {
  return {
    ground: cssColor("--ground"),
    grid: cssColor("--grid"),
    plot: cssColor("--plot"),
    plotEdge: cssColor("--plot-edge"),
    porch: cssColor("--porch"),
    porchEdge: cssColor("--porch-edge"),
    ink: cssColor("--ink"),
    muted: cssColor("--muted"),
    crit: cssColor("--crit"),
    input: cssColor("--input"),
    ok: cssColor("--ok"),
    done: cssColor("--done"),
    idle: cssColor("--idle"),
    accent: cssColor("--accent"),
    projects: [1, 2, 3, 4, 5, 6].map((i) => cssColor(`--p${i}`)),
  };
}

interface Rect {
  x: number;
  y: number;
  w: number;
  h: number;
}

interface District extends Rect {
  key: string;
  name: string;
  color: number;
}

interface Body {
  x: number;
  y: number;
  tx: number;
  ty: number;
  phase: number;
  dwell: number;
  moving: boolean;
  face: number;
  alpha: number;
  label: Text;
  glyph: Text;
}

interface Spark {
  x: number;
  y: number;
  vx: number;
  vy: number;
  life: number;
  color: number;
}

const DW = 300;
const DH = 220;
const GAP = 28;
const MARGIN = 40;
const PORCH_H = 140;
const DOCK_W = 230;
const FONT = '"JetBrains Mono", "Cascadia Mono", Consolas, monospace';

/** FNV-1a, for stable per-agent and per-project choices. */
function hash(s: string): number {
  let h = 0x811c9dc5;
  for (let i = 0; i < s.length; i++) {
    h ^= s.charCodeAt(i);
    h = Math.imul(h, 0x01000193);
  }
  return h >>> 0;
}

const rand = (a: number, b: number) => a + Math.random() * (b - a);
const ATTENTION: AgentState[] = ["needs_input", "awaiting_reply", "blocked", "crashed"];

export class World {
  selected: string | null = null;
  private app = new Application();
  private world = new Container();
  private ground = new Graphics();
  private groundText = new Container();
  private actors = new Graphics();
  private labels = new Container();
  private pal!: Palette;
  private districts = new Map<string, District>();
  private districtLabels = new Map<string, Text>();
  /** Estimated spend today, right-aligned in the district header. */
  private districtCosts = new Map<string, Text>();
  private projectOrder: string[] = [];
  private bodies = new Map<string, Body>();
  private sparks: Spark[] = [];
  private porch: Rect = { x: 0, y: 0, w: 0, h: 0 };
  private dock: Rect = { x: 0, y: 0, w: 0, h: 0 };
  private worldW = 0;
  private worldH = 0;
  private cam = { s: 1, x: 0, y: 0, user: false };
  private t = 0;
  private reduced = matchMedia("(prefers-reduced-motion: reduce)").matches;

  constructor(
    private daemon: Daemon,
    private onSelect: (id: string | null) => void,
  ) {}

  async init(host: HTMLElement): Promise<void> {
    this.pal = readPalette();
    await this.app.init({
      resizeTo: host,
      antialias: true,
      background: this.pal.ground,
      resolution: window.devicePixelRatio || 1,
      autoDensity: true,
    });
    host.appendChild(this.app.canvas);
    this.world.addChild(this.ground, this.groundText, this.actors, this.labels);
    this.app.stage.addChild(this.world);
    this.app.ticker.add((tk) => this.frame(Math.min(0.05, tk.deltaMS / 1000)));
    this.bindInput(this.app.canvas);
    matchMedia("(prefers-color-scheme: dark)").addEventListener("change", () => {
      this.pal = readPalette();
      this.app.renderer.background.color = this.pal.ground;
      this.layout();
    });
    this.daemon.onChange(() => this.sync());
    this.sync();
  }

  /** Return to the whole-colony view. */
  fit(): void {
    this.cam.user = false;
  }

  /** The map's container changed size without the window resizing. */
  resize(): void {
    this.app.resize();
  }

  /** Center the camera on an agent without changing zoom. */
  focus(id: string): void {
    const b = this.bodies.get(id);
    if (!b) return;
    const { width, height } = this.app.screen;
    this.cam.user = true;
    this.cam.x = width / 2 - b.x * this.cam.s;
    this.cam.y = height / 2 - b.y * this.cam.s;
  }

  // ---- data -> layout ------------------------------------------------------

  private projectOf(a: Agent): string {
    return a.project_key ?? "unknown";
  }

  private sync(): void {
    const agents = this.daemon.agents;
    const keys = new Set<string>();
    for (const a of agents.values()) keys.add(this.projectOf(a));
    const before = this.projectOrder.join("|");
    this.projectOrder = this.projectOrder.filter((k) => keys.has(k));
    for (const k of [...keys].sort()) if (!this.projectOrder.includes(k)) this.projectOrder.push(k);
    if (this.projectOrder.join("|") !== before || this.districts.size === 0) this.layout();

    for (const a of agents.values()) {
      const b = this.bodies.get(a.id);
      if (!b) this.bodies.set(a.id, this.newBody(a));
      else if (b.label.text !== a.name) b.label.text = a.name;
    }
    for (const [id, b] of this.bodies) {
      if (!agents.has(id)) {
        b.label.destroy();
        b.glyph.destroy();
        this.bodies.delete(id);
        if (this.selected === id) this.onSelect(null);
      }
    }
    this.updateDistrictLabels();
  }

  private newBody(a: Agent): Body {
    const parent = a.parent_id ? this.bodies.get(a.parent_id) : undefined;
    const start = parent ?? this.home(a);
    const label = new Text({ text: a.name, style: { fontFamily: FONT, fontSize: 11, fill: this.pal.muted } });
    label.anchor.set(0.5, 0);
    label.resolution = 3;
    const glyph = new Text({ text: "", style: { fontFamily: FONT, fontSize: 12, fontWeight: "700", fill: 0xffffff } });
    glyph.anchor.set(0.5, 0.5);
    glyph.resolution = 3;
    this.labels.addChild(label, glyph);
    return {
      x: start.x,
      y: start.y,
      tx: start.x,
      ty: start.y,
      phase: Math.random() * 6,
      dwell: 0,
      moving: false,
      face: 0,
      alpha: 1,
      label,
      glyph,
    };
  }

  private layout(): void {
    const n = Math.max(1, this.projectOrder.length);
    const cols = Math.min(n, Math.max(1, Math.ceil(Math.sqrt(n * 1.5))));
    const rows = Math.ceil(n / cols);
    const districtsW = cols * DW + (cols - 1) * GAP;
    const porchW = Math.min(640, Math.max(400, districtsW * 0.55));
    const bottomW = porchW + GAP + DOCK_W;
    this.worldW = Math.max(districtsW, bottomW);
    const left = (this.worldW - districtsW) / 2;
    this.districts.clear();
    this.projectOrder.forEach((key, i) => {
      const sample = [...this.daemon.agents.values()].find((a) => this.projectOf(a) === key);
      this.districts.set(key, {
        key,
        name: sample?.project_name ?? "unknown",
        color: this.pal.projects[hash(key) % this.pal.projects.length],
        x: left + (i % cols) * (DW + GAP),
        y: Math.floor(i / cols) * (DH + GAP),
        w: DW,
        h: DH,
      });
    });
    const by = rows * (DH + GAP) + 12;
    const bx = (this.worldW - bottomW) / 2;
    this.porch = { x: bx, y: by, w: porchW, h: PORCH_H };
    this.dock = { x: bx + porchW + GAP, y: by, w: DOCK_W, h: PORCH_H };
    this.worldH = by + PORCH_H;
    // Re-home idle bots in their (possibly moved) districts.
    for (const [id, b] of this.bodies) {
      const a = this.daemon.agents.get(id);
      if (a && (a.state === "idle" || a.state === "ended")) {
        const h = this.home(a);
        b.tx = h.x;
        b.ty = h.y;
      }
    }
    this.drawGround();
  }

  private home(a: Agent): { x: number; y: number } {
    const d = this.districts.get(this.projectOf(a));
    if (!d) return { x: this.worldW / 2, y: 0 };
    const h = hash(a.id);
    return { x: d.x + 30 + (h % (d.w - 60)), y: d.y + 70 + ((h >>> 12) % (d.h - 100)) };
  }

  private spot(d: District): { x: number; y: number } {
    return { x: rand(d.x + 28, d.x + d.w - 28), y: rand(d.y + 64, d.y + d.h - 24) };
  }

  private updateDistrictLabels(): void {
    for (const [key, text] of this.districtLabels) {
      const agents = [...this.daemon.agents.values()].filter((a) => this.projectOf(a) === key && a.kind === "main");
      const hosts = [...new Set(agents.map((a) => (a.host === "win" ? "Windows" : a.host.replace("wsl:", "WSL · "))))];
      const working = agents.filter((a) => a.state === "working").length;
      text.text = `${hosts.join(" + ") || "—"} · ${agents.length} session${agents.length === 1 ? "" : "s"} · ${working} working`;
    }
    for (const [key, text] of this.districtCosts) {
      const spent = spentToday(this.daemon.spend, this.daemon.now(), key);
      text.text = spent.tokens > 0 ? `${money(spent.usd)} today${spent.partial ? " · partial" : ""}` : "";
    }
  }

  private drawGround(): void {
    const g = this.ground;
    const p = this.pal;
    g.clear();
    for (const t of this.groundText.removeChildren()) t.destroy();
    this.districtCosts.clear();
    this.districtLabels.clear();
    const x0 = -MARGIN;
    const y0 = -MARGIN;
    for (let x = x0; x < this.worldW + MARGIN; x += 24) {
      for (let y = y0; y < this.worldH + MARGIN; y += 24) g.rect(x, y, 1.4, 1.4);
    }
    g.fill({ color: p.grid });
    const title = (text: string, x: number, y: number, color = p.ink, size = 13, weight: "600" | "400" = "600") => {
      const t = new Text({ text, style: { fontFamily: FONT, fontSize: size, fontWeight: weight, fill: color } });
      t.position.set(x, y);
      t.resolution = 3;
      this.groundText.addChild(t);
      return t;
    };
    for (const d of this.districts.values()) {
      g.roundRect(d.x, d.y, d.w, d.h, 12).fill({ color: p.plot }).stroke({ width: 1.5, color: p.plotEdge });
      g.roundRect(d.x + 14, d.y + 16, 10, 10, 2).fill({ color: d.color });
      title(d.name, d.x + 32, d.y + 13);
      this.districtLabels.set(d.key, title("", d.x + 14, d.y + 34, p.muted, 11, "400"));
      const cost = title("", d.x + d.w - 14, d.y + 14, p.muted, 11, "400");
      cost.anchor.set(1, 0);
      this.districtCosts.set(d.key, cost);
    }
    g.roundRect(this.porch.x, this.porch.y, this.porch.w, this.porch.h, 12)
      .fill({ color: p.porch })
      .stroke({ width: 1.5, color: p.porchEdge });
    title("FRONT PORCH · needs you", this.porch.x + 14, this.porch.y + 12, p.ink, 12);
    g.roundRect(this.dock.x, this.dock.y, this.dock.w, this.dock.h, 12)
      .fill({ color: p.plot })
      .stroke({ width: 1.5, color: p.plotEdge });
    title("REVIEW DOCK", this.dock.x + 14, this.dock.y + 12, p.ink, 12);
    if (this.projectOrder.length === 0) {
      title("No Claude Code sessions yet. Start one in any terminal or the desktop app.", 0, 90, p.muted, 12, "400");
    }
    this.updateDistrictLabels();
  }

  // ---- simulation -----------------------------------------------------------

  private frame(dt: number): void {
    this.t += dt;
    const agents = this.daemon.agents;
    const porchMain = this.daemon.porch().filter((a) => a.kind === "main");
    const crit = porchMain.filter((a) => a.state === "blocked" || a.state === "crashed");
    const input = porchMain.filter((a) => a.state === "needs_input" || a.state === "awaiting_reply");
    const ready = [...agents.values()]
      .filter((a) => a.kind === "main" && a.state === "ready_to_review")
      .sort((a, b) => a.state_since - b.state_since);

    for (const [id, b] of this.bodies) {
      const a = agents.get(id);
      if (!a) continue;
      this.steer(a, b, dt, crit, input, ready);
    }
    for (let i = this.sparks.length - 1; i >= 0; i--) {
      const s = this.sparks[i];
      s.life -= dt;
      s.x += s.vx * dt;
      s.y += s.vy * dt;
      s.vy += 60 * dt;
      if (s.life <= 0) this.sparks.splice(i, 1);
    }
    this.applyCamera();
    this.draw(ready);
  }

  private steer(a: Agent, b: Body, dt: number, crit: Agent[], input: Agent[], ready: Agent[]): void {
    b.phase += dt;
    const d = this.districts.get(this.projectOf(a));
    const near = (r = 3) => Math.hypot(b.tx - b.x, b.ty - b.y) < r;
    let speed = 55;
    let alpha = 1;
    const isMain = a.kind === "main";
    switch (a.state) {
      case "working":
        if (d && near()) {
          b.dwell -= dt;
          if (!this.reduced && Math.random() < dt * 3) {
            this.sparks.push({ x: b.x + rand(-6, 6), y: b.y - 16, vx: rand(-10, 10), vy: rand(-32, -16), life: 0.6, color: this.pal.ok });
          }
          if (b.dwell <= 0) {
            const s = this.spot(d);
            b.tx = s.x;
            b.ty = s.y;
            b.dwell = rand(1.5, 3.5);
          }
        }
        break;
      case "spawning":
        if (d) {
          b.tx = d.x + d.w / 2;
          b.ty = d.y + 70;
        }
        speed = 35;
        break;
      case "needs_input":
      case "awaiting_reply":
      case "blocked":
      case "crashed":
        if (isMain) {
          const front = a.state === "blocked" || a.state === "crashed";
          const row = front ? crit : input;
          const i = Math.max(0, row.findIndex((x) => x.id === a.id));
          const perRow = Math.max(1, Math.floor((this.porch.w - 50) / 42));
          const slotX = this.porch.x + 34 + (i % perRow) * 42;
          const slotY = front ? this.porch.y + this.porch.h - 30 : this.porch.y + 66;
          // Blocked bots pace in front of their slot; others stand still.
          const pace = a.state === "blocked" && !this.reduced && Math.hypot(slotX - b.x, slotY - b.y) < 16;
          b.tx = pace ? slotX + Math.sin(this.t * 2.2 + b.phase) * 10 : slotX;
          b.ty = slotY;
          speed = 85;
        } else {
          b.tx = b.x;
          b.ty = b.y;
        }
        break;
      case "ready_to_review": {
        const i = Math.max(0, ready.findIndex((x) => x.id === a.id));
        b.tx = this.dock.x + 30 + (i % 5) * 40;
        b.ty = this.dock.y + 62 + Math.floor(i / 5) * 40;
        speed = 60;
        break;
      }
      case "idle": {
        const h = this.home(a);
        if (near()) {
          b.dwell -= dt;
          if (b.dwell <= 0) {
            b.tx = h.x + rand(-18, 18);
            b.ty = h.y + rand(-10, 10);
            b.dwell = rand(5, 10);
          }
        } else if (Math.hypot(h.x - b.tx, h.y - b.ty) > 30) {
          b.tx = h.x;
          b.ty = h.y;
        }
        speed = 18;
        break;
      }
      case "ended": {
        if (!isMain && a.parent_id) {
          // A returned subagent walks back to its parent and merges into it.
          const p = this.bodies.get(a.parent_id);
          if (p) {
            b.tx = p.x;
            b.ty = p.y;
          }
          speed = 95;
          alpha = near(8) ? 0 : 1;
        } else {
          const h = this.home(a);
          b.tx = h.x;
          b.ty = h.y;
          speed = 18;
          alpha = 0.35;
        }
        break;
      }
    }
    b.alpha += (alpha - b.alpha) * Math.min(1, dt * 4);
    const dx = b.tx - b.x;
    const dy = b.ty - b.y;
    const dist = Math.hypot(dx, dy);
    if (dist > 0.5) {
      const step = Math.min(dist, speed * dt);
      b.x += (dx / dist) * step;
      b.y += (dy / dist) * step;
      b.face = dx / dist;
      b.moving = true;
    } else {
      b.moving = false;
    }
  }

  // ---- drawing --------------------------------------------------------------

  private stateColor(s: AgentState): number {
    const p = this.pal;
    switch (s) {
      case "working":
        return p.ok;
      case "needs_input":
      case "awaiting_reply":
        return p.input;
      case "blocked":
      case "crashed":
        return p.crit;
      case "ready_to_review":
        return p.done;
      default:
        return p.idle;
    }
  }

  private draw(ready: Agent[]): void {
    const g = this.actors;
    const agents = this.daemon.agents;
    g.clear();

    // Tethers from parents to their working subagents.
    for (const [id, b] of this.bodies) {
      const a = agents.get(id);
      if (!a || a.kind !== "subagent" || !a.parent_id || b.alpha < 0.05) continue;
      const p = this.bodies.get(a.parent_id);
      if (!p) continue;
      const mx = (b.x + p.x) / 2;
      const my = (b.y + p.y) / 2 + Math.min(30, Math.hypot(b.x - p.x, b.y - p.y) * 0.15);
      const color = ATTENTION.includes(a.state) ? this.stateColor(a.state) : this.districtColor(a);
      g.moveTo(p.x, p.y - 4).quadraticCurveTo(mx, my, b.x, b.y - 3).stroke({ width: 1.5, color, alpha: 0.75 * b.alpha });
    }

    // Packages waiting on the dock.
    ready.forEach((_, i) => {
      const x = this.dock.x + 22 + (i % 5) * 40;
      const y = this.dock.y + 74 + Math.floor(i / 5) * 40;
      g.roundRect(x, y, 16, 12, 2).fill({ color: this.pal.done });
      g.rect(x + 7, y, 2, 12).fill({ color: this.pal.plot });
    });

    const order = [...this.bodies.entries()].sort((a, b) => a[1].y - b[1].y);
    for (const [id, b] of order) {
      const a = agents.get(id);
      if (a) this.drawBot(g, a, b);
    }
    for (const s of this.sparks) g.rect(s.x - 1.5, s.y - 1.5, 3, 3).fill({ color: s.color, alpha: Math.max(0, s.life / 0.6) });
  }

  private districtColor(a: Agent): number {
    return this.districts.get(this.projectOf(a))?.color ?? this.pal.idle;
  }

  private drawBot(g: Graphics, a: Agent, b: Body): void {
    const p = this.pal;
    const sub = a.kind === "subagent";
    const r = sub ? 6 : 10;
    const st = a.state;
    const al = b.alpha;
    const resting = st === "idle" || st === "ended";
    const bob = this.reduced ? 0 : b.moving ? Math.abs(Math.sin(b.phase * 10)) * 2 : Math.sin(b.phase * 2) * 0.6;
    const x = b.x;
    const y = b.y - bob;
    const sc = this.stateColor(st);
    const body = this.districtColor(a);

    // Status ring: the most reliable signal at any zoom.
    const pulse = st === "blocked" && !this.reduced ? 1 + Math.sin(this.t * 8) * 0.15 : 1;
    g.ellipse(b.x, b.y + r * 0.7, r * 1.5 * pulse, r * 0.55 * pulse).fill({ color: sc, alpha: (resting ? 0.35 : 0.6) * al });

    if (st === "crashed") {
      // Lying on its side, eyes crossed out.
      g.ellipse(x, y, r * 1.3, r * 0.7).fill({ color: body, alpha: 0.5 * al }).stroke({ width: 1.2, color: p.ink, alpha: al });
      for (const s of [-1, 1]) {
        const ex = x + s * r * 0.45;
        g.moveTo(ex - 2, y - 4).lineTo(ex + 2, y).moveTo(ex + 2, y - 4).lineTo(ex - 2, y).stroke({ width: 1.3, color: p.ink, alpha: al });
      }
      this.setGlyph(b, "✕", p.crit, x, y - r - 12, al);
      this.placeLabel(a, b, r, al);
      return;
    }

    // Arms.
    const arm = (side: number, ang: number) => {
      g.moveTo(x + side * r * 0.8, y - r * 0.1)
        .lineTo(x + side * r * 0.8 + Math.cos(ang) * r * 0.9 * side, y - r * 0.1 + Math.sin(ang) * r * 0.9)
        .stroke({ width: sub ? 1.2 : 1.8, color: p.ink, alpha: al, cap: "round" });
    };
    if (st === "blocked") {
      const w = this.reduced ? 0 : Math.sin(this.t * 12) * 0.6;
      arm(-1, -1.9 + w);
      arm(1, -1.9 - w);
    } else if (st === "needs_input" || st === "awaiting_reply") {
      const up = Math.sin(this.t * 2.4 + b.phase) > 0.3;
      arm(-1, 0.9);
      arm(1, up ? -1.6 : 0.9);
    } else if (st === "working") {
      const w = this.reduced ? 0 : Math.sin(this.t * 9 + b.phase) * 0.5;
      arm(-1, 0.6);
      arm(1, 0.2 + w);
    } else {
      arm(-1, 1.1);
      arm(1, 1.1);
    }

    // Body and eyes.
    const squash = resting && !b.moving ? 0.85 : 1;
    g.ellipse(x, y - r * 0.2, r, r * squash).fill({ color: body, alpha: (resting ? 0.6 : 1) * al }).stroke({ width: 1.2, color: p.ink, alpha: al });
    const lookingAtYou = ATTENTION.includes(st);
    const lx = lookingAtYou ? 0 : b.face * 1.2;
    const ly = lookingAtYou ? 1.2 : 0;
    const er = sub ? 1.7 : 2.7;
    for (const s of [-1, 1]) {
      const ex = x + s * r * 0.38;
      const ey = y - r * 0.35;
      g.circle(ex, ey, er).fill({ color: 0xffffff, alpha: al });
      if (resting) g.rect(ex - er, ey - er, er * 2, er * 1.1).fill({ color: body, alpha: al });
      g.circle(ex + lx * er * 0.35, ey + ly * er * 0.35, er * 0.5).fill({ color: 0x111111, alpha: al });
    }

    // Overhead: one signal at a time, by priority.
    const oy = y - r - 13;
    const kids = a.children.filter((c) => {
      const k = this.daemon.agents.get(c);
      return k && k.state !== "ended";
    }).length;
    if (st === "blocked") {
      if (this.reduced || Math.sin(this.t * 7) > -0.3) g.circle(x, oy, sub ? 5.5 : 8).fill({ color: p.crit, alpha: al });
      this.setGlyph(b, "!", 0xffffff, x, oy, al);
    } else if (st === "needs_input" || st === "awaiting_reply") {
      g.roundRect(x + r * 0.5, oy - 9, 14, 16, 2).fill({ color: p.input, alpha: al });
      this.setGlyph(b, st === "awaiting_reply" ? "?" : "⚿", p.ink, x + r * 0.5 + 7, oy - 1, al);
    } else if (st === "ready_to_review") {
      g.moveTo(x + r, y - r).lineTo(x + r, oy - 8).stroke({ width: 1.2, color: p.ink, alpha: al });
      g.poly([x + r, oy - 8, x + r + 12, oy - 4, x + r, oy]).fill({ color: p.done, alpha: al });
      g.roundRect(x - 7, y + 1, 14, 10, 2).fill({ color: p.done, alpha: al });
      this.setGlyph(b, "", p.ink, x, oy, 0);
    } else if (st === "working" && kids > 0) {
      this.setGlyph(b, `×${kids}`, p.ink, x, oy + 2, al);
    } else if (st === "idle" && !b.moving && !this.reduced) {
      this.setGlyph(b, "z", p.muted, x + r, oy + 2, (0.5 + 0.5 * Math.sin(this.t * 1.5 + b.phase)) * al);
    } else {
      this.setGlyph(b, "", p.ink, x, oy, 0);
    }

    if (this.selected === a.id) g.circle(b.x, b.y - r * 0.2, r + 6).stroke({ width: 2, color: p.accent });
    this.placeLabel(a, b, r, al);
  }

  private setGlyph(b: Body, text: string, color: number, x: number, y: number, alpha: number): void {
    if (b.glyph.text !== text) b.glyph.text = text;
    if (b.glyph.style.fill !== color) b.glyph.style.fill = color;
    b.glyph.position.set(x, y);
    b.glyph.alpha = alpha;
  }

  private placeLabel(a: Agent, b: Body, r: number, alpha: number): void {
    const show = a.kind === "main" || this.selected === a.id || this.cam.s > 1.6;
    b.label.visible = show;
    b.label.position.set(b.x, b.y + r + 4);
    b.label.alpha = alpha;
  }

  // ---- camera and input ------------------------------------------------------

  private applyCamera(): void {
    const { width, height } = this.app.screen;
    if (!this.cam.user) {
      const s = Math.min(width / (this.worldW + MARGIN * 2), height / (this.worldH + MARGIN * 2));
      this.cam.s = Math.max(0.3, Math.min(2, s));
      this.cam.x = (width - this.worldW * this.cam.s) / 2;
      this.cam.y = (height - this.worldH * this.cam.s) / 2;
    }
    this.world.scale.set(this.cam.s);
    this.world.position.set(this.cam.x, this.cam.y);
  }

  private toWorld(ev: MouseEvent, el: HTMLElement): { x: number; y: number } {
    const r = el.getBoundingClientRect();
    return { x: (ev.clientX - r.left - this.cam.x) / this.cam.s, y: (ev.clientY - r.top - this.cam.y) / this.cam.s };
  }

  private pick(w: { x: number; y: number }): string | null {
    let best: string | null = null;
    let bd = 18;
    for (const [id, b] of this.bodies) {
      if (b.alpha < 0.1) continue;
      const d = Math.hypot(b.x - w.x, b.y - 4 - w.y);
      if (d < bd) {
        bd = d;
        best = id;
      }
    }
    return best;
  }

  private bindInput(el: HTMLCanvasElement): void {
    let drag: { x: number; y: number; cx: number; cy: number; moved: boolean } | null = null;
    el.addEventListener("pointerdown", (e) => {
      drag = { x: e.clientX, y: e.clientY, cx: this.cam.x, cy: this.cam.y, moved: false };
      el.setPointerCapture(e.pointerId);
    });
    el.addEventListener("pointermove", (e) => {
      if (!drag) return;
      const dx = e.clientX - drag.x;
      const dy = e.clientY - drag.y;
      if (!drag.moved && Math.hypot(dx, dy) < 4) return;
      drag.moved = true;
      this.cam.user = true;
      this.cam.x = drag.cx + dx;
      this.cam.y = drag.cy + dy;
    });
    el.addEventListener("pointerup", (e) => {
      const wasDrag = drag?.moved;
      drag = null;
      if (wasDrag) return;
      const id = this.pick(this.toWorld(e, el));
      this.onSelect(id);
    });
    el.addEventListener("dblclick", (e) => {
      const id = this.pick(this.toWorld(e, el));
      const a = id ? this.daemon.agents.get(id) : undefined;
      const d = a ? this.districts.get(this.projectOf(a)) : undefined;
      if (!d) return;
      const { width, height } = this.app.screen;
      this.cam.user = true;
      this.cam.s = Math.min(2.5, Math.min(width / (d.w + 80), height / (d.h + 80)));
      this.cam.x = width / 2 - (d.x + d.w / 2) * this.cam.s;
      this.cam.y = height / 2 - (d.y + d.h / 2) * this.cam.s;
    });
    el.addEventListener(
      "wheel",
      (e) => {
        e.preventDefault();
        const r = el.getBoundingClientRect();
        const mx = e.clientX - r.left;
        const my = e.clientY - r.top;
        const s = Math.max(0.25, Math.min(3, this.cam.s * Math.exp(-e.deltaY * 0.0015)));
        this.cam.x = mx - ((mx - this.cam.x) * s) / this.cam.s;
        this.cam.y = my - ((my - this.cam.y) * s) / this.cam.s;
        this.cam.s = s;
        this.cam.user = true;
      },
      { passive: false },
    );
  }
}

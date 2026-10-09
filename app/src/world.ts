// The colony map: districts per project, the front porch, the review dock,
// and one bot per agent. Agent state comes from the daemon (truth layer);
// everything here is presentation, and must never contradict it.

import { Application, Container, Graphics, Particle, ParticleContainer, Text, type Texture } from "pixi.js";
import type { Daemon } from "./daemon";
import { prefs } from "./prefs";
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
  /** Focus-mode dimming, 1 = fully shown. Eased so toggling fades. */
  dim: number;
  /** The current record; replaced on every daemon update. */
  a: Agent;
  /** Texts are made on first use: most bots never show one at far zoom. */
  label: Text | null;
  glyph: Text | null;
  /** Changed-file counts under a package on the dock. */
  diff: Text | null;
  /** Cached home spot, cleared when the layout changes. */
  home: { x: number; y: number } | null;
  inView: boolean;
  /** Far-zoom dot: two tinted particles sharing one texture, drawn in a single call for the lot. */
  ring: Particle | null;
  dot: Particle | null;
  /** The building it is working at, when it has one. */
  dir: string;
}

/** A directory agents are working in, drawn inside its project's district. */
interface Building {
  key: string;
  slot: number;
  seen: number;
  label: Text;
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
/** The dock is taller than the porch: packages carry a line of changed-file counts. */
const DOCK_H = 168;
const DOCK_ROW = 64;
/** Buildings per district: one per directory agents work in, at most this many. */
const MAX_BUILDINGS = 6;
/** A building stays this long after the last agent worked in it. */
const BUILDING_TTL_MS = 5 * 60_000;
const WARN = 0xf97316;
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
/** Only bots at work show a collision: ones on the porch or dock are not editing. */
const EDITING: AgentState[] = ["working"];
const ATTENTION: AgentState[] = ["needs_input", "awaiting_reply", "blocked", "crashed"];
/** Attention budget: healthy work stays under this ceiling. Alarm states may exceed it. */
const HEALTHY = { swingRate: 4, swingAmp: 0.25, stepRate: 6, stepBob: 1.5, sparkRate: 1 };
/** In Focus mode, what is not asking for you fades to this. */
const FOCUS_DIM = 0.18;
const FOCUS_DIM_REVIEW = 0.5;
/** Level of detail by how many bots are on screen (with hysteresis) and zoom:
 *  0 full bots, 1 plain bodies (no arms, eyes or outlines), 2 status dots. */
const LOD_FULL_MAX = 100;
const LOD_FULL_BACK = 85;
const LOD_PLAIN_ZOOM = 0.75;
/** Buildings change slowly: redraw them this often, not every frame. */
const BUILDING_REDRAW_MS = 250;

export class World {
  selected: string | null = null;
  private app = new Application();
  private world = new Container();
  private ground = new Graphics();
  private groundText = new Container();
  private buildingsG = new Graphics();
  private dots!: ParticleContainer;
  private dotTex!: Texture;
  private lastBuildingDraw = 0;
  private buildingsDirty = true;
  private actors = new Graphics();
  private dirty = true;
  private tier = 0;
  /** prefs.reduced asks the browser each time: read once per frame. */
  private reducedNow = false;
  private readyList: Agent[] = [];
  /** Position in its row (porch or dock) by agent id. */
  private slotOf = new Map<string, number>();
  /** Live subagents per agent id. */
  private kidsOf = new Map<string, number>();
  /** Draw order, kept sorted by y with insertion sort (bots move little per frame). */
  private order: Body[] = [];
  private pairs = new Set<string>();
  private view = { x0: 0, y0: 0, x1: 0, y1: 0 };
  private labels = new Container();
  private pal!: Palette;
  private districts = new Map<string, District>();
  private districtLabels = new Map<string, Text>();
  /** Estimated spend today, right-aligned in the district header. */
  private districtCosts = new Map<string, Text>();
  private projectOrder: string[] = [];
  private bodies = new Map<string, Body>();
  private buildingLabels = new Container();
  /** District key -> directory key -> building. */
  private buildings = new Map<string, Map<string, Building>>();
  /** "district|dir" -> bots working there, and whether any is in an edit collision. */
  private occupancy = new Map<string, { n: number; hot: boolean }>();
  private lastPrune = 0;
  private sparks: Spark[] = [];
  private porch: Rect = { x: 0, y: 0, w: 0, h: 0 };
  private dock: Rect = { x: 0, y: 0, w: 0, h: 0 };
  private worldW = 0;
  private worldH = 0;
  private cam = { s: 1, x: 0, y: 0, user: false };
  private t = 0;
  /** OS setting or the in-app override (see prefs.ts). */
  private get reduced(): boolean {
    return this.reducedNow;
  }

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
    this.world.addChild(this.ground, this.groundText, this.buildingsG, this.buildingLabels, this.actors, this.labels);
    this.dotTex = this.app.renderer.generateTexture({ target: new Graphics().circle(16, 16, 16).fill(0xffffff), resolution: 2 });
    this.dots = new ParticleContainer({ texture: this.dotTex, dynamicProperties: { position: true, color: true } });
    this.world.addChildAt(this.dots, this.world.getChildIndex(this.labels));
    this.app.stage.addChild(this.world);
    this.app.ticker.add((tk) => this.frame(Math.min(0.05, tk.deltaMS / 1000)));
    this.bindInput(this.app.canvas);
    matchMedia("(prefers-color-scheme: dark)").addEventListener("change", () => {
      this.pal = readPalette();
      this.app.renderer.background.color = this.pal.ground;
      this.layout();
    });
    prefs.onChange(() => {
      this.applyFocusLayers();
      this.buildingsDirty = true;
    });
    this.applyFocusLayers();
    // Messages can arrive hundreds of times a second: fold them into one sync per frame.
    this.daemon.onChange(() => (this.dirty = true));
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

    let membership = false;
    for (const a of agents.values()) {
      const b = this.bodies.get(a.id);
      if (!b) {
        this.bodies.set(a.id, this.newBody(a));
        membership = true;
      } else {
        b.a = a;
        if (b.label && b.label.text !== a.name) b.label.text = a.name;
      }
    }
    for (const [id, b] of this.bodies) {
      if (!agents.has(id)) {
        b.label?.destroy();
        b.glyph?.destroy();
        b.diff?.destroy();
        if (b.ring) this.dots.removeParticle(b.ring);
        if (b.dot) this.dots.removeParticle(b.dot);
        this.bodies.delete(id);
        membership = true;
        if (this.selected === id) this.onSelect(null);
      }
    }
    if (membership) this.order = [...this.bodies.values()];
    this.derive();
    this.updateBuildings(Date.now());
    this.updateDistrictLabels();
    this.buildingsDirty = true;
  }

  /** Per-update lists the simulation needs every frame, built once per change. */
  private derive(): void {
    const crit: Agent[] = [];
    const input: Agent[] = [];
    const ready: Agent[] = [];
    this.kidsOf.clear();
    for (const a of this.daemon.agents.values()) {
      if (a.kind === "main") {
        if (a.state === "blocked" || a.state === "crashed") crit.push(a);
        else if (a.state === "needs_input" || a.state === "awaiting_reply") input.push(a);
        else if (a.state === "ready_to_review") ready.push(a);
      }
      if (a.parent_id && a.state !== "ended") this.kidsOf.set(a.parent_id, (this.kidsOf.get(a.parent_id) ?? 0) + 1);
    }
    const bySince = (x: Agent, y: Agent) => x.state_since - y.state_since;
    crit.sort(bySince);
    input.sort(bySince);
    ready.sort(bySince);
    this.readyList = ready;
    this.slotOf.clear();
    for (const row of [crit, input, ready]) row.forEach((a, i) => this.slotOf.set(a.id, i));
  }

  // ---- buildings: one per directory agents work in ---------------------------

  /** The building for a directory in a district, or the one for its top folder
   *  when the district is full and the directory got folded into it. */
  private buildingFor(district: string, dir: string): Building | undefined {
    const m = this.buildings.get(district);
    return m?.get(dir) ?? m?.get(dir.split("/")[0]);
  }

  /** Open buildings where bots are working; close ones nobody has used lately. */
  private updateBuildings(now: number): void {
    for (const a of this.daemon.agents.values()) {
      if (a.state !== "working" || a.work_dir == null) continue;
      const district = this.projectOf(a);
      let m = this.buildings.get(district);
      if (!m) this.buildings.set(district, (m = new Map()));
      let b = this.buildingFor(district, a.work_dir);
      if (!b) {
        // Over the limit, fold into the top folder so a project stays a handful of buildings.
        let key = a.work_dir;
        if (m.size >= MAX_BUILDINGS) {
          // Close one nobody is using; if all are busy, fold into the top folder.
          const idle = [...m.values()].filter((x) => now - x.seen > 5000).sort((x, y) => x.seen - y.seen)[0];
          const top = a.work_dir.split("/")[0];
          if (idle) this.closeBuilding(m, idle);
          else if (m.has(top)) key = top;
          else this.closeBuilding(m, [...m.values()].sort((x, y) => x.seen - y.seen)[0]);
          b = m.get(key);
        }
        b ??= this.openBuilding(m, key, now);
      }
      b.seen = now;
    }
    for (const [district, m] of this.buildings) {
      for (const b of [...m.values()]) if (now - b.seen > BUILDING_TTL_MS) this.closeBuilding(m, b);
      if (m.size === 0) this.buildings.delete(district);
    }
    this.recount();
    this.buildingsDirty = true;
  }

  /** Which buildings have bots working in them (and any in an edit collision). */
  private recount(): void {
    this.occupancy.clear();
    for (const a of this.daemon.agents.values()) {
      if (a.state !== "working" || a.work_dir == null) continue;
      const district = this.projectOf(a);
      const bld = this.buildingFor(district, a.work_dir);
      if (!bld) continue;
      const k = `${district}|${bld.key}`;
      const o = this.occupancy.get(k) ?? { n: 0, hot: false };
      o.n++;
      o.hot ||= !!a.collision;
      this.occupancy.set(k, o);
    }
  }

  private openBuilding(m: Map<string, Building>, key: string, now: number): Building {
    const used = new Set([...m.values()].map((b) => b.slot));
    let slot = 0;
    while (used.has(slot)) slot++;
    const label = new Text({ text: key === "" ? "(project root)" : key.length > 16 ? `…${key.slice(-15)}` : key, style: { fontFamily: FONT, fontSize: 9, fill: this.pal.muted } });
    label.anchor.set(0.5, 0);
    label.resolution = 3;
    this.buildingLabels.addChild(label);
    const b = { key, slot, seen: now, label };
    m.set(key, b);
    return b;
  }

  private closeBuilding(m: Map<string, Building>, b: Building | undefined): void {
    if (!b) return;
    b.label.destroy();
    m.delete(b.key);
  }

  /** Where a building stands inside its district: three across, two deep, below the header. */
  private plot(d: District, slot: number): { x: number; y: number; w: number; h: number; door: { x: number; y: number } } {
    const cw = (d.w - 24) / 3;
    const cx = d.x + 12 + (slot % 3) * cw + cw / 2;
    const top = d.y + 66 + Math.floor(slot / 3) * 76;
    return { x: cx - 26, y: top + 8, w: 52, h: 28, door: { x: cx, y: top + 46 } };
  }

  private newBody(a: Agent): Body {
    const parent = a.parent_id ? this.bodies.get(a.parent_id) : undefined;
    const start = parent ?? this.home(a);
    return {
      a,
      home: null,
      inView: true,
      ring: null,
      dot: null,
      x: start.x,
      y: start.y,
      tx: start.x,
      ty: start.y,
      phase: Math.random() * 6,
      dwell: 0,
      moving: false,
      face: 0,
      alpha: 1,
      dim: 1,
      label: null,
      glyph: null,
      diff: null,
      dir: "",
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
    this.dock = { x: bx + porchW + GAP, y: by, w: DOCK_W, h: DOCK_H };
    this.worldH = by + Math.max(PORCH_H, DOCK_H);
    // Re-home idle bots in their (possibly moved) districts.
    for (const b of this.bodies.values()) b.home = null;
    this.buildingsDirty = true;
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
    const b = this.bodies.get(a.id);
    if (b?.home) return b.home;
    const d = this.districts.get(this.projectOf(a));
    if (!d) return { x: this.worldW / 2, y: 0 };
    const h = hash(a.id);
    const spot = { x: d.x + 30 + (h % (d.w - 60)), y: d.y + 70 + ((h >>> 12) % (d.h - 100)) };
    if (b) b.home = spot;
    return spot;
  }

  private spot(d: District): { x: number; y: number } {
    return { x: rand(d.x + 28, d.x + d.w - 28), y: rand(d.y + 64, d.y + d.h - 24) };
  }

  /** A spot at a building's door. */
  private doorSpot(d: District, bld: Building): { x: number; y: number } {
    const door = this.plot(d, bld.slot).door;
    return { x: door.x + rand(-26, 26), y: door.y + rand(-4, 12) };
  }

  private updateDistrictLabels(): void {
    const stats = new Map<string, { hosts: Set<string>; n: number; working: number }>();
    for (const a of this.daemon.agents.values()) {
      if (a.kind !== "main") continue;
      const k = this.projectOf(a);
      let st = stats.get(k);
      if (!st) stats.set(k, (st = { hosts: new Set(), n: 0, working: 0 }));
      st.hosts.add(a.host === "win" ? "Windows" : a.host.replace("wsl:", "WSL · "));
      st.n++;
      if (a.state === "working") st.working++;
    }
    for (const [key, text] of this.districtLabels) {
      const st = stats.get(key);
      const next = `${st ? [...st.hosts].join(" + ") : "—"} · ${st?.n ?? 0} session${st?.n === 1 ? "" : "s"} · ${st?.working ?? 0} working`;
      if (text.text !== next) text.text = next;
    }
    for (const [key, text] of this.districtCosts) {
      const spent = spentToday(this.daemon.spend, this.daemon.now(), key);
      const next = spent.tokens > 0 ? `${money(spent.usd)} today${spent.partial ? " · partial" : ""}` : "";
      if (text.text !== next) text.text = next;
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

  // ---- focus mode ----------------------------------------------------------

  /** Whether the bot is asking for the user (always shown at full strength in Focus mode). */
  private needsYou(a: Agent): boolean {
    return ATTENTION.includes(a.state) || this.selected === a.id;
  }

  /** Dim target for a bot: 1 unless Focus mode is on and it can wait. */
  private dimTarget(a: Agent): number {
    if (!prefs.focus || this.needsYou(a)) return 1;
    return a.state === "ready_to_review" ? FOCUS_DIM_REVIEW : FOCUS_DIM;
  }

  /** Not needing you in Focus mode: held still as well as dimmed. */
  private calm(a: Agent): boolean {
    return this.reduced || (prefs.focus && !this.needsYou(a));
  }

  private applyFocusLayers(): void {
    this.groundText.alpha = prefs.focus ? 0.45 : 1;
    this.buildingLabels.alpha = prefs.focus ? 0.3 : 1;
  }

  // ---- simulation -----------------------------------------------------------

  private frame(dt: number): void {
    this.t += dt;
    this.reducedNow = prefs.reduced;
    if (this.dirty) {
      this.dirty = false;
      this.sync();
    }
    const nowMs = Date.now();
    if (nowMs - this.lastPrune > 1000) {
      this.lastPrune = nowMs;
      this.updateBuildings(nowMs);
    }

    this.applyCamera();
    this.cull();
    for (const b of this.order) this.steer(b.a, b, dt);
    for (let i = this.sparks.length - 1; i >= 0; i--) {
      const s = this.sparks[i];
      s.life -= dt;
      s.x += s.vx * dt;
      s.y += s.vy * dt;
      s.vy += 60 * dt;
      if (s.life <= 0) this.sparks.splice(i, 1);
    }
    this.draw();
  }

  /** Which bots are on screen, and the level of detail that follows from how many. */
  private cull(): void {
    const { width, height } = this.app.screen;
    const s = this.cam.s;
    const pad = 40;
    const v = this.view;
    v.x0 = -this.cam.x / s - pad;
    v.y0 = -this.cam.y / s - pad - 30;
    v.x1 = (width - this.cam.x) / s + pad;
    v.y1 = (height - this.cam.y) / s + pad;
    let n = 0;
    for (const b of this.order) {
      b.inView = b.x >= v.x0 && b.x <= v.x1 && b.y >= v.y0 && b.y <= v.y1;
      if (b.inView) n++;
    }
    const full = this.tier === 0 ? n <= LOD_FULL_MAX : n <= LOD_FULL_BACK;
    this.tier = full ? 0 : s >= LOD_PLAIN_ZOOM ? 1 : 2;
    // Nearly sorted already, so insertion sort is linear.
    const o = this.order;
    for (let i = 1; i < o.length; i++) {
      const x = o[i];
      let j = i - 1;
      while (j >= 0 && o[j].y > x.y) {
        o[j + 1] = o[j];
        j--;
      }
      o[j + 1] = x;
    }
  }

  private steer(a: Agent, b: Body, dt: number): void {
    b.phase += dt;
    const dim = this.dimTarget(a);
    b.dim = this.reduced ? dim : b.dim + (dim - b.dim) * Math.min(1, dt * 6);
    const d = this.districts.get(this.projectOf(a));
    const near = (r = 3) => Math.hypot(b.tx - b.x, b.ty - b.y) < r;
    let speed = 55;
    let alpha = 1;
    const isMain = a.kind === "main";
    switch (a.state) {
      case "working": {
        // Heading for the building of the directory it works in, if it has one.
        const bld = d && a.work_dir != null ? this.buildingFor(d.key, a.work_dir) : undefined;
        if (d && bld && b.dir !== bld.key) {
          b.dir = bld.key;
          const s = this.doorSpot(d, bld);
          b.tx = s.x;
          b.ty = s.y;
          b.dwell = rand(1.5, 3.5);
        } else if (!bld) {
          b.dir = "";
        }
        if (d && near()) {
          b.dwell -= dt;
          if (b.inView && this.tier < 2 && !this.calm(a) && Math.random() < dt * HEALTHY.sparkRate) {
            this.sparks.push({ x: b.x + rand(-6, 6), y: b.y - 16, vx: rand(-10, 10), vy: rand(-32, -16), life: 0.6, color: this.pal.ok });
          }
          if (b.dwell <= 0) {
            const s = bld ? this.doorSpot(d, bld) : this.spot(d);
            b.tx = s.x;
            b.ty = s.y;
            b.dwell = rand(1.5, 3.5);
          }
        }
        break;
      }
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
          const i = this.slotOf.get(a.id) ?? 0;
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
        const i = this.slotOf.get(a.id) ?? 0;
        b.tx = this.dock.x + 30 + (i % 5) * 40;
        b.ty = this.dock.y + 62 + Math.floor(i / 5) * DOCK_ROW;
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

  private draw(): void {
    const g = this.actors;
    const agents = this.daemon.agents;
    g.clear();
    const nowMs = Date.now();
    if (this.buildingsDirty || nowMs - this.lastBuildingDraw > BUILDING_REDRAW_MS) {
      this.buildingsDirty = false;
      this.lastBuildingDraw = nowMs;
      this.buildingsG.clear();
      this.drawBuildings(this.buildingsG);
    }

    // Edit collisions: a dashed line between the bots, whatever they are doing
    // (far zoom colours the bot's ring instead).
    this.pairs.clear();
    if (this.tier < 2) for (const b of this.order) {
      const a = b.a;
      if (!a.collision || !EDITING.includes(a.state)) continue;
      for (const w of a.collision.with) {
        const o = this.bodies.get(w);
        // From either side (the two lists can differ), once per pair.
        const pair = a.id < w ? `${a.id}|${w}` : `${w}|${a.id}`;
        if (this.pairs.has(pair)) continue;
        this.pairs.add(pair);
        // Only while both are still at it, in one district: stale warnings stay off the map.
        if (!o || !EDITING.includes(agents.get(w)?.state ?? "ended") || this.projectOf(agents.get(w)!) !== this.projectOf(a)) continue;
        this.dashed(g, b.x, b.y - 4, o.x, o.y - 4, b.alpha * o.alpha * Math.max(b.dim, o.dim));
      }
    }

    // Tethers from parents to their working subagents (far zoom shows districts, not threads).
    if (this.tier < 2) {
      for (const b of this.order) {
        const a = b.a;
        if (a.kind !== "subagent" || !a.parent_id || b.alpha < 0.05) continue;
        const p = this.bodies.get(a.parent_id);
        if (!p || !(b.inView || p.inView)) continue;
        const mx = (b.x + p.x) / 2;
        const my = (b.y + p.y) / 2 + Math.min(30, Math.hypot(b.x - p.x, b.y - p.y) * 0.15);
        const color = ATTENTION.includes(a.state) ? this.stateColor(a.state) : this.districtColor(a);
        g.moveTo(p.x, p.y - 4).quadraticCurveTo(mx, my, b.x, b.y - 3).stroke({ width: 1.5, color, alpha: 0.75 * b.alpha * b.dim });
      }
    }

    // Packages waiting on the dock.
    const pa = prefs.focus ? FOCUS_DIM_REVIEW : 1;
    for (let i = 0; i < this.readyList.length; i++) {
      const x = this.dock.x + 22 + (i % 5) * 40;
      const y = this.dock.y + 74 + Math.floor(i / 5) * DOCK_ROW;
      g.roundRect(x, y, 16, 12, 2).fill({ color: this.pal.done, alpha: pa });
      g.rect(x + 7, y, 2, 12).fill({ color: this.pal.plot, alpha: pa });
    }

    for (const b of this.order) {
      if (b.inView) this.drawBot(g, b.a, b);
      else this.hideTexts(b);
    }
    for (const s of this.sparks) g.rect(s.x - 1.5, s.y - 1.5, 3, 3).fill({ color: s.color, alpha: Math.max(0, s.life / 0.6) });
  }

  private drawBuildings(g: Graphics): void {
    const p = this.pal;
    const zoomedIn = this.cam.s > 0.55;
    const nowMs = Date.now();
    for (const [district, m] of this.buildings) {
      const d = this.districts.get(district);
      for (const bld of m.values()) {
        if (!d) {
          bld.label.visible = false;
          continue;
        }
        const r = this.plot(d, bld.slot);
        const occ = this.occupancy.get(`${district}|${bld.key}`);
        const n = occ?.n ?? 0;
        const hot = occ?.hot ?? false;
        const fade = Math.max(0.45, 1 - (nowMs - bld.seen) / BUILDING_TTL_MS) * (prefs.focus && !hot ? FOCUS_DIM * 1.5 : 1);
        g.poly([r.x - 4, r.y, r.x + r.w / 2, r.y - 10, r.x + r.w + 4, r.y]).fill({ color: d.color, alpha: 0.85 * fade });
        g.roundRect(r.x, r.y, r.w, r.h, 2)
          .fill({ color: d.color, alpha: (n > 0 ? 0.5 : 0.22) * fade })
          .stroke({ width: hot ? 2 : 1.2, color: hot ? WARN : p.ink, alpha: hot ? 1 : 0.7 * fade });
        g.rect(r.x + r.w / 2 - 4, r.y + r.h - 9, 8, 9).fill({ color: p.ink, alpha: 0.5 * fade });
        // A lit window per bot working inside, up to three.
        for (let i = 0; i < Math.min(3, n); i++) g.rect(r.x + 6 + i * 14, r.y + 5, 8, 6).fill({ color: p.ok });
        if (hot) {
          const tx = r.x + r.w + 2;
          const ty = r.y - 14;
          g.poly([tx, ty + 12, tx + 7, ty, tx + 14, ty + 12]).fill({ color: WARN }).stroke({ width: 1, color: p.ink });
          g.rect(tx + 6.2, ty + 4, 1.6, 4).fill({ color: p.ink });
          g.rect(tx + 6.2, ty + 9, 1.6, 1.6).fill({ color: p.ink });
        }
        bld.label.visible = zoomedIn;
        bld.label.position.set(r.x + r.w / 2, r.y + r.h + 3);
        bld.label.alpha = fade;
      }
    }
  }

  private dashed(g: Graphics, x1: number, y1: number, x2: number, y2: number, alpha: number): void {
    const len = Math.hypot(x2 - x1, y2 - y1);
    if (len < 1) return;
    const ux = (x2 - x1) / len;
    const uy = (y2 - y1) / len;
    for (let t = (this.reduced ? 0 : (this.t * 14) % 10); t < len; t += 10) {
      const e = Math.min(len, t + 5);
      g.moveTo(x1 + ux * t, y1 + uy * t).lineTo(x1 + ux * e, y1 + uy * e);
    }
    g.stroke({ width: 1.8, color: WARN, alpha: 0.9 * alpha });
  }

  private districtColor(a: Agent): number {
    return this.districts.get(this.projectOf(a))?.color ?? this.pal.idle;
  }

  private arm(g: Graphics, x: number, y: number, r: number, sub: boolean, al: number, side: number, ang: number): void {
    g.moveTo(x + side * r * 0.8, y - r * 0.1)
      .lineTo(x + side * r * 0.8 + Math.cos(ang) * r * 0.9 * side, y - r * 0.1 + Math.sin(ang) * r * 0.9)
      .stroke({ width: sub ? 1.2 : 1.8, color: this.pal.ink, alpha: al });
  }

  private drawBot(g: Graphics, a: Agent, b: Body): void {
    const p = this.pal;
    const sub = a.kind === "subagent";
    const r = sub ? 6 : 10;
    const st = a.state;
    const al = b.alpha * b.dim;
    const resting = st === "idle" || st === "ended";
    const calm = this.calm(a);
    const tier = this.tier;
    const bob = calm ? 0 : b.moving ? Math.abs(Math.sin(b.phase * HEALTHY.stepRate)) * HEALTHY.stepBob : Math.sin(b.phase * 2) * 0.6;
    const x = b.x;
    const y = b.y - bob;
    const sc = this.stateColor(st);
    const body = this.districtColor(a);

    if (tier === 2) {
      // Far zoom: a dot in the district's colour inside a status-coloured ring, nothing else.
      // Alarm states keep their glyph so they are never lost in the crowd.
      this.setDot(b, x, y, r * 1.25, a.collision && EDITING.includes(st) ? WARN : sc, (resting ? 0.5 : 0.9) * al, r * 0.8, body, (resting ? 0.6 : 1) * al);
      if (st === "blocked" || st === "crashed") this.setGlyph(b, st === "crashed" ? "✕" : "!", 0xffffff, x, y, al);
      else this.setGlyph(b, "", p.ink, x, y, 0);
      if (this.selected === a.id) g.circle(x, y, r + 6).stroke({ width: 2, color: p.accent });
      this.placeLabel(a, b, r, al);
      return;
    }

    this.hideDot(b);
    // Status ring: the most reliable signal at any zoom.
    const pulse = st === "blocked" && !this.reduced ? 1 + Math.sin(this.t * 8) * 0.15 : 1;
    g.ellipse(b.x, b.y + r * 0.7, r * 1.5 * pulse, r * 0.55 * pulse).fill({ color: sc, alpha: (resting ? 0.35 : 0.6) * al });

    if (st === "crashed") {
      // Lying on its side, eyes crossed out.
      g.ellipse(x, y, r * 1.3, r * 0.7).fill({ color: body, alpha: 0.5 * al }).stroke({ width: 1.2, color: p.ink, alpha: al });
      if (tier === 0) {
        for (const s of [-1, 1]) {
          const ex = x + s * r * 0.45;
          g.moveTo(ex - 2, y - 4).lineTo(ex + 2, y).moveTo(ex + 2, y - 4).lineTo(ex - 2, y).stroke({ width: 1.3, color: p.ink, alpha: al });
        }
      }
      this.setGlyph(b, "✕", p.crit, x, y - r - 12, al);
      this.placeLabel(a, b, r, al);
      return;
    }

    if (tier === 0) {
      // Arms.
      if (st === "blocked") {
        const w = this.reduced ? 0 : Math.sin(this.t * 12) * 0.6;
        this.arm(g, x, y, r, sub, al, -1, -1.9 + w);
        this.arm(g, x, y, r, sub, al, 1, -1.9 - w);
      } else if (st === "needs_input" || st === "awaiting_reply") {
        const up = this.reduced || Math.sin(this.t * 2.4 + b.phase) > 0.3;
        this.arm(g, x, y, r, sub, al, -1, 0.9);
        this.arm(g, x, y, r, sub, al, 1, up ? -1.6 : 0.9);
      } else if (st === "working") {
        const w = calm ? 0 : Math.sin(this.t * HEALTHY.swingRate + b.phase) * HEALTHY.swingAmp;
        this.arm(g, x, y, r, sub, al, -1, 0.6);
        this.arm(g, x, y, r, sub, al, 1, 0.2 + w);
      } else {
        this.arm(g, x, y, r, sub, al, -1, 1.1);
        this.arm(g, x, y, r, sub, al, 1, 1.1);
      }
    }

    // Body and eyes.
    const squash = resting && !b.moving ? 0.85 : 1;
    const bodyShape = g.ellipse(x, y - r * 0.2, r, r * squash).fill({ color: body, alpha: (resting ? 0.6 : 1) * al });
    if (tier === 0) {
      bodyShape.stroke({ width: 1.2, color: p.ink, alpha: al });
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
    }

    // Overhead: one signal at a time, by priority.
    const oy = y - r - 13;
    const kids = this.kidsOf.get(a.id) ?? 0;
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
    } else if (a.paused_at) {
      this.setGlyph(b, "⏸", p.muted, x + r, oy + 2, al);
    } else if (st === "idle" && !b.moving && !calm) {
      this.setGlyph(b, "z", p.muted, x + r, oy + 2, (0.5 + 0.5 * Math.sin(this.t * 1.5 + b.phase)) * al);
    } else {
      this.setGlyph(b, "", p.ink, x, oy, 0);
    }

    // Edit collision: a small warning sign beside the head, whatever else it signals.
    if (a.collision && EDITING.includes(st)) {
      const wx = x - r - 8;
      const wy = oy - 2;
      g.poly([wx - 6, wy + 6, wx, wy - 6, wx + 6, wy + 6]).fill({ color: WARN, alpha: al }).stroke({ width: 1, color: p.ink, alpha: al });
      g.rect(wx - 0.7, wy - 2, 1.4, 4.5).fill({ color: p.ink, alpha: al });
    }

    if (this.selected === a.id) g.circle(b.x, b.y - r * 0.2, r + 6).stroke({ width: 2, color: p.accent });
    this.placeLabel(a, b, r, al);
  }

  private text(size: number, fill: number, weight?: "700"): Text {
    const t = new Text({ text: "", style: { fontFamily: FONT, fontSize: size, fontWeight: weight ?? "400", fill } });
    t.anchor.set(0.5, weight ? 0.5 : 0);
    t.resolution = 3;
    this.labels.addChild(t);
    return t;
  }

  private setGlyph(b: Body, text: string, color: number, x: number, y: number, alpha: number): void {
    if (text === "" || alpha <= 0) {
      if (b.glyph) b.glyph.visible = false;
      return;
    }
    const gl = (b.glyph ??= this.text(12, 0xffffff, "700"));
    gl.visible = true;
    if (gl.text !== text) gl.text = text;
    if (gl.style.fill !== color) gl.style.fill = color;
    gl.position.set(x, y);
    gl.alpha = alpha;
  }

  /** A bot off screen keeps no visible text. */
  /** Radii are fixed per bot (main or subagent), so the particle's size is set once. */
  private setDot(b: Body, x: number, y: number, r1: number, c1: number, a1: number, r2: number, c2: number, a2: number): void {
    if (!b.ring || !b.dot) {
      const make = (r: number) => new Particle({ texture: this.dotTex, x, y, scaleX: r / 16, scaleY: r / 16, anchorX: 0.5, anchorY: 0.5 });
      b.ring = make(r1);
      b.dot = make(r2);
      this.dots.addParticle(b.ring);
      this.dots.addParticle(b.dot);
    }
    b.ring.x = b.dot.x = x;
    b.ring.y = b.dot.y = y;
    b.ring.tint = c1;
    b.dot.tint = c2;
    b.ring.alpha = a1;
    b.dot.alpha = a2;
  }

  private hideDot(b: Body): void {
    if (b.ring && b.ring.alpha !== 0) b.ring.alpha = b.dot!.alpha = 0;
  }

  private hideTexts(b: Body): void {
    this.hideDot(b);
    if (b.label) b.label.visible = false;
    if (b.glyph) b.glyph.visible = false;
    if (b.diff) b.diff.visible = false;
  }

  private placeLabel(a: Agent, b: Body, r: number, alpha: number): void {
    const show = this.selected === a.id || (this.tier < 2 && (a.kind === "main" || this.cam.s > 1.6));
    if (!show) {
      if (b.label) b.label.visible = false;
      if (b.diff) b.diff.visible = false;
      return;
    }
    const label = (b.label ??= this.text(11, this.pal.muted));
    if (label.text !== a.name) label.text = a.name;
    label.visible = true;
    label.position.set(b.x, b.y + r + 4);
    label.alpha = alpha;
    const ds = a.state === "ready_to_review" ? a.diff_stat : null;
    if (ds) {
      const diff = (b.diff ??= this.text(9, this.pal.muted));
      const text = `${ds.files}f +${ds.added} −${ds.removed}`;
      if (diff.text !== text) diff.text = text;
      diff.position.set(b.x, b.y + r + 16);
      diff.alpha = alpha;
      diff.visible = true;
    } else if (b.diff) b.diff.visible = false;
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

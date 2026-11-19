// Draggable dividers: map | inspector (width, or height on a phone) and map / terminal (height).
// Sizes live in CSS variables and are remembered between runs. Double-click
// a divider to go back to the default; arrow keys nudge it when focused.
import { platform } from "./platform";

const KEY = "colony.layout";

interface Saved {
  inspector?: number;
  inspectorH?: number;
  terminal?: number;
}

function load(): Saved {
  try {
    return JSON.parse(localStorage.getItem(KEY) ?? "{}") as Saved;
  } catch {
    return {};
  }
}

function save(patch: Saved): void {
  try {
    localStorage.setItem(KEY, JSON.stringify({ ...load(), ...patch }));
  } catch {
    // Private window or blocked storage: the layout just won't be remembered.
  }
}

/** One way a divider can resize things; a divider may have more than one (the side panel is a column, or a sheet on a phone). */
interface Axis {
  /** CSS variable the size lives in. */
  prop: string;
  /** The element whose size is being set, for clamping against its container. */
  container: HTMLElement;
  /** The panel being sized. */
  target: HTMLElement;
  /** True when the size is a width. */
  horizontal: boolean;
  min: number;
  /** Largest share of the container. */
  maxShare: number;
  /** Size from a pointer position: the divider's neighbour grows as the pointer moves away. */
  size: (e: PointerEvent, box: DOMRect) => number;
  /** Arrow keys: [shrink, grow]. */
  keys: [string, string];
  key: keyof Saved;
}

/** Phone layout: the side panel sits under the map, so its divider is a horizontal bar. */
const PHONE = "(max-width: 760px)";

function wire(handle: HTMLElement, pick: () => Axis, saved: Saved): void {
  const root = document.documentElement;
  const apply = (a: Axis, px: number | null) => {
    if (px == null) root.style.removeProperty(a.prop);
    else root.style.setProperty(a.prop, `${Math.round(px)}px`);
  };
  const limit = (a: Axis, px: number) => {
    const box = a.container.getBoundingClientRect();
    const total = a.horizontal ? box.width : box.height;
    return Math.max(a.min, Math.min(px, Math.max(a.min, total * a.maxShare)));
  };
  const current = (a: Axis) => {
    const v = parseFloat(root.style.getPropertyValue(a.prop));
    if (Number.isFinite(v)) return v;
    const r = a.target.getBoundingClientRect();
    return a.horizontal ? r.width : r.height;
  };

  // Restore every size this divider can set; only the one in use takes effect.
  for (const a of axes(pick)) {
    const s = saved[a.key];
    if (s) apply(a, s);
  }

  // The axis is fixed for the length of a drag, even if the window turns while it runs.
  let active: Axis | null = null;
  handle.addEventListener("pointerdown", (e) => {
    e.preventDefault();
    active = pick();
    handle.setPointerCapture(e.pointerId);
    handle.classList.add("dragging");
    document.body.classList.add("resizing");
  });
  handle.addEventListener("pointermove", (e) => {
    if (!active || !handle.hasPointerCapture(e.pointerId)) return;
    apply(active, limit(active, active.size(e, active.container.getBoundingClientRect())));
  });
  const end = (e: PointerEvent) => {
    if (!active || !handle.hasPointerCapture(e.pointerId)) return;
    handle.releasePointerCapture(e.pointerId);
    handle.classList.remove("dragging");
    document.body.classList.remove("resizing");
    save({ [active.key]: Math.round(current(active)) });
    active = null;
  };
  handle.addEventListener("pointerup", end);
  handle.addEventListener("pointercancel", end);
  // A double tap on a phone counts too.
  handle.addEventListener("dblclick", () => {
    const a = pick();
    apply(a, null);
    save({ [a.key]: undefined });
  });
  handle.addEventListener("keydown", (e) => {
    const a = pick();
    const dir = e.key === a.keys[0] ? -1 : e.key === a.keys[1] ? 1 : 0;
    if (!dir) return;
    e.preventDefault();
    apply(a, limit(a, current(a) + dir * (e.shiftKey ? 96 : 24)));
    save({ [a.key]: Math.round(current(a)) });
  });
  // A smaller window can leave a remembered size too big: keep it in bounds.
  window.addEventListener("resize", () => {
    const a = pick();
    if (root.style.getPropertyValue(a.prop)) apply(a, limit(a, current(a)));
  });
}

/** Both axes a divider can have, by asking `pick` under each layout. */
function axes(pick: () => Axis): Axis[] {
  return [pick()];
}

export function initLayout(): void {
  // Android's status bar can sit on top of the page; the stylesheet leaves room for it under html.mobile.
  if (platform().kind === "mobile") document.documentElement.classList.add("mobile");
  const saved = load();
  const stage = document.querySelector<HTMLElement>(".stage")!;
  const mapCol = document.querySelector<HTMLElement>(".map-col")!;
  const inspector = document.getElementById("inspector")!;
  const termPane = document.getElementById("term-pane")!;
  const phone = window.matchMedia(PHONE);
  const column: Axis = {
    prop: "--insp-w",
    container: stage,
    target: inspector,
    horizontal: true,
    min: 320,
    maxShare: 0.7,
    // The inspector is on the right: its width is what is left of the pointer.
    size: (e, box) => box.right - e.clientX,
    keys: ["ArrowRight", "ArrowLeft"],
    key: "inspector",
  };
  const sheet: Axis = {
    prop: "--insp-h",
    container: stage,
    target: inspector,
    horizontal: false,
    min: 120,
    maxShare: 0.8,
    // The inspector is below the map: its height is what is left under the pointer.
    size: (e, box) => box.bottom - e.clientY,
    keys: ["ArrowDown", "ArrowUp"],
    key: "inspectorH",
  };
  wire(document.getElementById("split-v")!, () => (phone.matches ? sheet : column), saved);
  // The phone's saved height has to be restored too: wire() only restores the axis in use.
  if (saved.inspectorH) document.documentElement.style.setProperty(sheet.prop, `${saved.inspectorH}px`);
  if (saved.inspector) document.documentElement.style.setProperty(column.prop, `${saved.inspector}px`);
  wire(
    document.getElementById("split-h")!,
    () => ({
      prop: "--term-h",
      container: mapCol,
      target: termPane,
      horizontal: false,
      min: 140,
      maxShare: 0.8,
      size: (e, box) => box.bottom - e.clientY,
      keys: ["ArrowDown", "ArrowUp"],
      key: "terminal",
    }),
    saved,
  );
}

// Draggable dividers: map | inspector (width) and map / terminal (height).
// Sizes live in CSS variables and are remembered between runs. Double-click
// a divider to go back to the default; arrow keys nudge it when focused.

const KEY = "colony.layout";

interface Saved {
  inspector?: number;
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

interface Axis {
  handle: HTMLElement;
  /** CSS variable the size lives in. */
  prop: string;
  /** The element whose size is being set, for clamping against its container. */
  container: HTMLElement;
  min: number;
  /** Largest share of the container. */
  maxShare: number;
  /** Size from a pointer position: the divider's neighbour grows as the pointer moves away. */
  size: (e: PointerEvent, box: DOMRect) => number;
  /** Arrow keys: [shrink, grow]. */
  keys: [string, string];
  key: keyof Saved;
}

function wire(a: Axis, saved: Saved): void {
  const root = document.documentElement;
  const apply = (px: number | null) => {
    if (px == null) root.style.removeProperty(a.prop);
    else root.style.setProperty(a.prop, `${Math.round(px)}px`);
  };
  const limit = (px: number) => {
    const box = a.container.getBoundingClientRect();
    const total = a.prop === "--insp-w" ? box.width : box.height;
    return Math.max(a.min, Math.min(px, Math.max(a.min, total * a.maxShare)));
  };
  const current = () => {
    const v = parseFloat(root.style.getPropertyValue(a.prop));
    if (Number.isFinite(v)) return v;
    const target = a.prop === "--insp-w" ? document.getElementById("inspector")! : document.getElementById("term-pane")!;
    const r = target.getBoundingClientRect();
    return a.prop === "--insp-w" ? r.width : r.height;
  };

  const start = saved[a.key];
  if (start) apply(start);

  a.handle.addEventListener("pointerdown", (e) => {
    e.preventDefault();
    a.handle.setPointerCapture(e.pointerId);
    a.handle.classList.add("dragging");
    document.body.classList.add("resizing");
  });
  a.handle.addEventListener("pointermove", (e) => {
    if (!a.handle.hasPointerCapture(e.pointerId)) return;
    apply(limit(a.size(e, a.container.getBoundingClientRect())));
  });
  const end = (e: PointerEvent) => {
    if (!a.handle.hasPointerCapture(e.pointerId)) return;
    a.handle.releasePointerCapture(e.pointerId);
    a.handle.classList.remove("dragging");
    document.body.classList.remove("resizing");
    save({ [a.key]: Math.round(current()) });
  };
  a.handle.addEventListener("pointerup", end);
  a.handle.addEventListener("pointercancel", end);
  a.handle.addEventListener("dblclick", () => {
    apply(null);
    save({ [a.key]: undefined });
  });
  a.handle.addEventListener("keydown", (e) => {
    const dir = e.key === a.keys[0] ? -1 : e.key === a.keys[1] ? 1 : 0;
    if (!dir) return;
    e.preventDefault();
    apply(limit(current() + dir * (e.shiftKey ? 96 : 24)));
    save({ [a.key]: Math.round(current()) });
  });
  // A smaller window can leave a remembered size too big: keep it in bounds.
  window.addEventListener("resize", () => {
    if (root.style.getPropertyValue(a.prop)) apply(limit(current()));
  });
}

export function initLayout(): void {
  const saved = load();
  const stage = document.querySelector<HTMLElement>(".stage")!;
  const mapCol = document.querySelector<HTMLElement>(".map-col")!;
  wire(
    {
      handle: document.getElementById("split-v")!,
      prop: "--insp-w",
      container: stage,
      min: 320,
      maxShare: 0.7,
      // The inspector is on the right: its width is what is left of the pointer.
      size: (e, box) => box.right - e.clientX,
      keys: ["ArrowRight", "ArrowLeft"],
      key: "inspector",
    },
    saved,
  );
  wire(
    {
      handle: document.getElementById("split-h")!,
      prop: "--term-h",
      container: mapCol,
      min: 140,
      maxShare: 0.8,
      size: (e, box) => box.bottom - e.clientY,
      keys: ["ArrowDown", "ArrowUp"],
      key: "terminal",
    },
    saved,
  );
}
